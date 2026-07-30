//! Screenshots and metadata for the comments under a community post.
//!
//! The feed says only how many comments a post has, so each post's own page is
//! opened and its comment section scrolled out. Only the comments clearing the
//! like threshold are screenshotted, since that filter is the difference
//! between a handful of captures per post and hundreds.
//!
//! Threads are captured a comment at a time rather than whole: the top-level
//! comment is its own clip, and with `--subcomments` the first few replies
//! under it are each another, nested under it in the index and written to a
//! folder named after the comment they answer. A thread is left collapsed
//! unless replies were asked for.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result};
use headless_chrome::{Browser, Tab};
use log::{debug, info, trace, warn};
use serde_json::Value;

use crate::browser::{self, ImageFormat};
use crate::clip::Clipper;
use crate::comments::meta::{self, Meta};
use crate::comments::{COMMENTS_JS, replies};

/// Screenshots go under `<out_dir>/comments/<post>/`, so the post captures
/// themselves stay a flat, sortable list.
pub(super) const COMMENTS_DIR: &str = "comments";

/// Rounds of scrolling that must find no new comments before we call it done.
const STABLE_ROUNDS: u32 = 3;
const MAX_SCROLLS: u32 = 200;

/// A post whose comments we want.
pub(super) struct Target {
	/// The post's position in the feed, which its screenshots are named after.
	pub index: usize,
	pub id: String,
}

/// What a run wants out of each post's comment section.
#[derive(Clone, Copy)]
pub(super) struct Wanted {
	/// Keep only comments with at least this many likes.
	pub min_likes: u64,
	/// Capture up to this many replies under each kept comment; zero leaves
	/// every thread collapsed.
	pub subcomments: usize,
	/// What the screenshots are encoded as.
	pub format: ImageFormat,
}

/// Collects the comments of every target, returning one JSON array per target,
/// in the same order. A post whose comment section could not be read comes back
/// as `Value::Null`, which tells a reader of the index that the post was tried
/// and nothing could be said about it — an empty array means it genuinely had
/// no comment clearing `min_likes`.
///
/// Each worker drives its own browser and takes posts off a shared cursor, so
/// one post with thousands of comments does not leave the others idle.
pub(super) fn collect(targets: &[Target], out_dir: &Path, wanted: Wanted, workers: usize) -> Vec<Value> {
	if targets.is_empty() {
		return Vec::new();
	}
	info!("collecting comments for {} posts", targets.len());

	let results = Mutex::new(vec![Value::Null; targets.len()]);
	let cursor = AtomicUsize::new(0);
	let threads = workers.clamp(1, targets.len());

	std::thread::scope(|scope| {
		for _ in 0..threads {
			scope.spawn(|| {
				let mut session: Option<Session> = None;
				loop {
					let i = cursor.fetch_add(1, Ordering::Relaxed);
					let Some(target) = targets.get(i) else { break };
					match run_post(&mut session, target, out_dir, wanted) {
						Ok(comments) => results.lock().expect("comments lock poisoned")[i] = comments,
						Err(e) => {
							warn!("no comments for post {}: {e:#}", target.index + 1);
							// The session may well be what failed; the next post
							// starts a fresh browser rather than inheriting it.
							session = None;
						}
					}
				}
			});
		}
	});

	results.into_inner().expect("comments lock poisoned")
}

/// One post: load its page, read every top-level comment, and capture the ones
/// clearing `min_likes` along with the replies asked for under each.
fn run_post(session: &mut Option<Session>, target: &Target, out_dir: &Path, wanted_opts: Wanted) -> Result<Value> {
	let Wanted {
		min_likes,
		subcomments,
		format,
	} = wanted_opts;
	let url = super::post_url(&target.id);
	let open = match session.take() {
		Some(open) => {
			open.goto(&url)?;
			open
		}
		None => Session::start(&url)?,
	};
	let session = session.insert(open);

	let clipper = Clipper::new(&session.tab, format);
	clipper.init_viewport()?;
	clipper.eval(COMMENTS_JS)?;
	clipper.eval("__yta.prepare()")?;
	if subcomments > 0 {
		clipper.eval("__yta.subcomments(true)")?;
	}

	let loaded = load_comments(&clipper)?;
	if loaded == 0 {
		if clipper.eval("__yta.disabled()")?.as_bool().unwrap_or(false) {
			debug!("post {} has comments turned off", target.index + 1);
		} else {
			debug!("post {} has no comments", target.index + 1);
		}
		return Ok(Value::Array(Vec::new()));
	}

	let metas: Vec<Meta> = clipper.eval("__yta.metas()")?.as_array().map(|list| list.iter().map(Meta::from_value).collect()).unwrap_or_default();

	let wanted: Vec<usize> = (0..metas.len()).filter(|&i| metas[i].likes >= min_likes).collect();
	info!("[{}] {} of {loaded} comments clear {min_likes} likes", target.index + 1, wanted.len());
	if wanted.is_empty() {
		return Ok(Value::Array(Vec::new()));
	}

	let rel_dir = format!("{COMMENTS_DIR}/{}", post_dir_name(target));
	let dir = out_dir.join(COMMENTS_DIR).join(post_dir_name(target));
	std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

	let mut entries = Vec::with_capacity(wanted.len());
	for (ordinal, &i) in wanted.iter().enumerate() {
		let meta = &metas[i];
		// Opening the threads before this one moves it down the capture list,
		// so what to capture is pointed out now rather than assumed.
		let Some(node) = replies::select(&clipper, i, None)? else {
			warn!("[{}] comment {} left the page", target.index + 1, ordinal + 1);
			continue;
		};
		let file = capture_file_name(ordinal, meta.id.as_deref(), format);
		let (file, captured) = match capture_node(&clipper, node, &file, &dir, &rel_dir) {
			Ok(found) => found,
			Err(e) => {
				// Losing the browser here costs us the rest of this post's
				// captures, so hand back what we have and let the caller move
				// on with a fresh session.
				if !clipper.is_alive() {
					warn!("browser session ended on post {} comment {}", target.index + 1, ordinal + 1);
					entries.push(entry(meta, &target.id, None));
					return Ok(Value::Array(entries));
				}
				warn!("[{}] comment {} skipped: {e:#}", target.index + 1, ordinal + 1);
				(None, None)
			}
		};
		// The capture expands a clamped body, so its text is the fuller one.
		let meta = match captured {
			Some(fresh) if fresh.text.is_some() => Meta {
				text: fresh.text,
				..meta.clone()
			},
			_ => meta.clone(),
		};

		let mut value = entry(&meta, &target.id, file.as_deref());
		if subcomments > 0 {
			let thread = Thread {
				ordinal,
				index: i,
				parent_id: meta.id.clone(),
			};
			let subs = capture_replies(&clipper, target, &dir, &rel_dir, &thread, subcomments, format);
			if let Some(value) = value.as_object_mut() {
				value.insert("subcomments".to_string(), Value::Array(subs));
			}
			if !clipper.is_alive() {
				warn!("browser session ended on post {} comment {} replies", target.index + 1, ordinal + 1);
				entries.push(value);
				return Ok(Value::Array(entries));
			}
		}
		entries.push(value);
	}
	Ok(Value::Array(entries))
}

/// The top-level comment a batch of replies hangs off.
struct Thread {
	/// Its place among the comments kept for this post, which its screenshot
	/// and the folder of replies under it are both named after.
	ordinal: usize,
	/// Its place among every thread on the page, which is how the page is asked
	/// about it.
	index: usize,
	parent_id: Option<String>,
}

/// Opens `thread`'s replies and captures up to `limit` of them, in the order
/// the page shows them, into a folder of their own under the parent comment's
/// name.
///
/// Best effort throughout: a thread that will not open, or a reply that will
/// not capture, costs only itself — the comment it hangs off is already
/// indexed. A caller that finds the browser gone afterwards should stop.
fn capture_replies(clipper: &Clipper, target: &Target, dir: &Path, rel_dir: &str, thread: &Thread, limit: usize, format: ImageFormat) -> Vec<Value> {
	let (ordinal, i) = (thread.ordinal, thread.index);
	let loaded = match replies::load(clipper, i, limit) {
		Ok(0) => return Vec::new(),
		Ok(loaded) => loaded,
		Err(e) => {
			warn!("[{}] comment {} replies did not open: {e:#}", target.index + 1, ordinal + 1);
			return Vec::new();
		}
	};

	let metas: Vec<Meta> = match replies::metas(clipper, i) {
		Ok(metas) => metas,
		Err(e) => {
			warn!("[{}] comment {} replies could not be read: {e:#}", target.index + 1, ordinal + 1);
			return Vec::new();
		}
	};
	let keep = metas.len().min(limit);
	debug!("[{}] comment {} has {loaded} replies loaded, keeping {keep}", target.index + 1, ordinal + 1);
	if keep == 0 {
		return Vec::new();
	}

	// Made only once there is something to put in it, so a thread whose replies
	// all fell through does not leave an empty folder behind.
	let name = replies::dir_name(ordinal, thread.parent_id.as_deref());
	let dir = dir.join(&name);
	if let Err(e) = std::fs::create_dir_all(&dir) {
		warn!("[{}] comment {} replies have nowhere to go: {e:#}", target.index + 1, ordinal + 1);
		return Vec::new();
	}
	let rel_dir = format!("{rel_dir}/{name}");

	let mut entries = Vec::with_capacity(keep);
	for (r, meta) in metas.iter().enumerate().take(keep) {
		let node = match replies::select(clipper, i, Some(r)) {
			Ok(Some(node)) => node,
			Ok(None) => {
				warn!("[{}] comment {} reply {} left the page", target.index + 1, ordinal + 1, r + 1);
				continue;
			}
			Err(e) => {
				warn!("[{}] comment {} reply {} skipped: {e:#}", target.index + 1, ordinal + 1, r + 1);
				continue;
			}
		};
		let file = capture_file_name(r, meta.id.as_deref(), format);
		let (file, captured) = match capture_node(clipper, node, &file, &dir, &rel_dir) {
			Ok(found) => found,
			Err(e) => {
				if !clipper.is_alive() {
					entries.push(entry(meta, &target.id, None));
					return entries;
				}
				warn!("[{}] comment {} reply {} skipped: {e:#}", target.index + 1, ordinal + 1, r + 1);
				(None, None)
			}
		};
		let meta = match captured {
			Some(fresh) if fresh.text.is_some() => Meta {
				text: fresh.text,
				..meta.clone()
			},
			_ => meta.clone(),
		};
		entries.push(entry(&meta, &target.id, file.as_deref()));
	}
	entries
}

/// Screenshots the element at `node`, returning the path recorded in the index
/// (relative to the output directory) and what the page said about it at
/// capture time.
fn capture_node(clipper: &Clipper, node: usize, file: &str, dir: &Path, rel_dir: &str) -> Result<(Option<String>, Option<Meta>)> {
	let (image, info) = clipper.capture(node)?;
	let path = dir.join(file);
	std::fs::write(&path, image).with_context(|| format!("writing {}", path.display()))?;

	let relative = format!("{rel_dir}/{file}");
	debug!("wrote {relative}");
	Ok((Some(relative), Some(Meta::from_value(&info["meta"]))))
}

/// Scrolls the post page until every top-level comment has loaded. Returns how
/// many there are.
fn load_comments(clipper: &Clipper) -> Result<usize> {
	let count = |clipper: &Clipper| -> Result<usize> { Ok(clipper.eval("__yta.count()")?.as_u64().unwrap_or(0) as usize) };

	let mut last = count(clipper)?;
	let mut stable = 0;
	for _ in 0..MAX_SCROLLS {
		let scrolled = clipper.eval("__yta.scrollToBottom()")?.as_u64().unwrap_or(0) as usize;
		sleep(Duration::from_millis(700));
		let now = scrolled.max(count(clipper)?);

		if now > last {
			trace!("loaded {now} comments...");
			last = now;
			stable = 0;
		} else {
			stable += 1;
			if stable >= STABLE_ROUNDS {
				break;
			}
		}
	}
	clipper.eval("window.scrollTo(0, 0)")?;
	Ok(last)
}

/// A browser kept alive across posts: the page is heavy to start but cheap to
/// navigate, and the dark-theme preference only has to be set once per profile.
struct Session {
	// Dropping the browser kills the tab, so it is held for its lifetime.
	_browser: Browser,
	tab: std::sync::Arc<Tab>,
}

impl Session {
	fn start(url: &str) -> Result<Self> {
		debug!("starting a browser session for comments");
		let browser = browser::launch()?;
		let tab = browser.new_tab()?;
		tab.set_default_timeout(Duration::from_secs(60));
		browser::emulate_dark(&tab)?;

		tab.navigate_to(url)?.wait_until_navigated()?;
		browser::set_dark_theme_cookie(&tab)?;
		tab.reload(false, None)?.wait_until_navigated()?;
		Ok(Self {
			_browser: browser,
			tab,
		})
	}

	fn goto(&self, url: &str) -> Result<()> {
		debug!("loading {url}");
		self.tab.navigate_to(url)?.wait_until_navigated()?;
		Ok(())
	}
}

/// The `posts.json` value for one comment, pointing at the comment on the
/// post's own page.
fn entry(meta: &Meta, post_id: &str, screenshot: Option<&str>) -> Value {
	let url = meta.id.as_ref().map(|id| format!("{}?lc={id}", super::post_url(post_id)));
	meta::entry(meta, url, screenshot)
}

/// `0001_UgkxAbC`, matching the post's own screenshot name so the two line up
/// when the directory is sorted.
fn post_dir_name(target: &Target) -> String {
	format!("{:04}_{}", target.index + 1, meta::sanitize(Some(&target.id)))
}

/// `001_UgwXyZ.png`, in the order the page showed the comments — for a reply
/// too, numbered within the thread it belongs to rather than across the post,
/// since it lives in a folder of that thread's own.
fn capture_file_name(ordinal: usize, id: Option<&str>, format: ImageFormat) -> String {
	format!("{:03}_{}.{}", ordinal + 1, meta::sanitize(id), format.extension())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn names_captures_after_the_post_they_belong_to() {
		let target = Target {
			index: 0,
			id: "Ugkx/../Abc".to_string(),
		};
		assert_eq!(post_dir_name(&target), "0001_UgkxAbc");
		assert_eq!(capture_file_name(0, Some("UgwXyZ"), ImageFormat::Png), "001_UgwXyZ.png");
		assert_eq!(capture_file_name(11, None, ImageFormat::Webp(80)), "012_unknown.webp");
	}

	#[test]
	fn points_an_entry_at_the_comment_on_the_posts_page() {
		let meta = Meta::from_value(&serde_json::json!({"id": "UgwXyZ", "likes": "1.2K"}));
		let value = entry(&meta, "UgkxAbc", None);
		assert_eq!(value["url"], "https://www.youtube.com/post/UgkxAbc?lc=UgwXyZ");
		assert_eq!(value["likes"], 1200);
	}
}
