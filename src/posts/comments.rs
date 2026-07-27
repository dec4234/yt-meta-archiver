//! Screenshots and metadata for the comments under a community post.
//!
//! The feed says only how many comments a post has, so each post's own page is
//! opened and its comment section scrolled out. Only top-level comments are
//! captured — a thread's replies hang off it and are deliberately left out of
//! the clip — and only those clearing the like threshold are screenshotted,
//! since that filter is the difference between a handful of captures per post
//! and hundreds.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result};
use headless_chrome::{Browser, Tab};
use log::{debug, info, trace, warn};
use serde_json::Value;

use super::clip::Clipper;
use crate::browser::{self, ImageFormat};

const COMMENTS_JS: &str = include_str!("js/comments.js");

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

/// Collects the comments of every target, returning one JSON array per target,
/// in the same order. A post whose comment section could not be read comes back
/// as `Value::Null`, which tells a reader of the index that the post was tried
/// and nothing could be said about it — an empty array means it genuinely had
/// no comment clearing `min_likes`.
///
/// Each worker drives its own browser and takes posts off a shared cursor, so
/// one post with thousands of comments does not leave the others idle.
pub(super) fn collect(targets: &[Target], out_dir: &Path, min_likes: u64, workers: usize, format: ImageFormat) -> Vec<Value> {
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
					match run_post(&mut session, target, out_dir, min_likes, format) {
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
/// clearing `min_likes`.
fn run_post(session: &mut Option<Session>, target: &Target, out_dir: &Path, min_likes: u64, format: ImageFormat) -> Result<Value> {
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

	let dir = out_dir.join(COMMENTS_DIR).join(post_dir_name(target));
	std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

	let mut entries = Vec::with_capacity(wanted.len());
	for (ordinal, &i) in wanted.iter().enumerate() {
		let meta = &metas[i];
		let (file, captured) = match capture_comment(&clipper, i, ordinal, meta, &dir, format) {
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
		entries.push(entry(&meta, &target.id, file.as_deref()));
	}
	Ok(Value::Array(entries))
}

/// Screenshots comment `i`, returning the path recorded in the index (relative
/// to the output directory) and what the page said about it at capture time.
fn capture_comment(clipper: &Clipper, i: usize, ordinal: usize, meta: &Meta, dir: &Path, format: ImageFormat) -> Result<(Option<String>, Option<Meta>)> {
	let (image, info) = clipper.capture(i)?;
	let file = comment_file_name(ordinal, meta.id.as_deref(), format);
	let path = dir.join(&file);
	std::fs::write(&path, image).with_context(|| format!("writing {}", path.display()))?;

	let relative = format!("{COMMENTS_DIR}/{}/{file}", dir.file_name().unwrap_or_default().to_string_lossy());
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

/// What the page says about one comment, as read off the DOM.
#[derive(Clone, Default)]
struct Meta {
	id: Option<String>,
	author: Option<String>,
	author_url: Option<String>,
	text: Option<String>,
	/// Likes as a number, from the count YouTube renders.
	likes: u64,
	/// That count as shown, e.g. "1.2K", kept because it is what the
	/// screenshot has in it.
	likes_text: Option<String>,
	replies: u64,
	published: Option<String>,
	pinned: bool,
	hearted: bool,
}

impl Meta {
	fn from_value(value: &Value) -> Self {
		let string = |key: &str| value[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
		// The accessible label spells the count out in full where the visible
		// one is abbreviated, so it is the better of the two when present.
		let likes_text = string("likes");
		let likes = string("likesLabel").as_deref().map(parse_count).filter(|n| *n > 0);

		Self {
			id: string("id"),
			author: string("author"),
			author_url: string("authorUrl"),
			text: string("text"),
			likes: likes.unwrap_or_else(|| likes_text.as_deref().map_or(0, parse_count)),
			likes_text,
			replies: string("replies").as_deref().map_or(0, parse_count),
			published: string("published"),
			pinned: value["pinned"].as_bool().unwrap_or(false),
			hearted: value["hearted"].as_bool().unwrap_or(false),
		}
	}
}

/// The `posts.json` value for one comment.
fn entry(meta: &Meta, post_id: &str, screenshot: Option<&str>) -> Value {
	serde_json::json!({
		"url": meta.id.as_ref().map(|id| format!("{}?lc={id}", super::post_url(post_id))),
		"author": meta.author,
		"author_url": meta.author_url,
		"text": meta.text,
		"likes": meta.likes,
		"likes_text": meta.likes_text,
		"replies": meta.replies,
		"pinned": meta.pinned,
		"hearted": meta.hearted,
		"posted_relative": meta.published,
		"screenshot": screenshot,
	})
}

/// `0001_UgkxAbC`, matching the post's own screenshot name so the two line up
/// when the directory is sorted.
fn post_dir_name(target: &Target) -> String {
	format!("{:04}_{}", target.index + 1, sanitize(Some(&target.id)))
}

/// `001_UgwXyZ.png`, in the order the page showed the comments.
fn comment_file_name(ordinal: usize, id: Option<&str>, format: ImageFormat) -> String {
	format!("{:03}_{}.{}", ordinal + 1, sanitize(id), format.extension())
}

/// A YouTube id reduced to what is safe in a file name.
fn sanitize(id: Option<&str>) -> String {
	id.map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
		.filter(|s| !s.is_empty())
		.unwrap_or_else(|| "unknown".to_string())
}

/// A count as YouTube renders it — "1.2K", "1,234", "3 replies" — as a number.
/// Anything unrecognisable counts as zero, which for a like filter errs towards
/// leaving a comment out rather than capturing the whole feed.
fn parse_count(raw: &str) -> u64 {
	let Some(token) = raw.split_whitespace().next() else {
		return 0;
	};
	let multiplier = match token.chars().last().map(|c| c.to_ascii_uppercase()) {
		Some('K') => 1_000.0,
		Some('M') => 1_000_000.0,
		Some('B') => 1_000_000_000.0,
		_ => 1.0,
	};
	let digits: String = token.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
	digits.parse::<f64>().map_or(0, |n| (n * multiplier) as u64)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_the_counts_youtube_renders() {
		assert_eq!(parse_count("12"), 12);
		assert_eq!(parse_count(" 1,234 "), 1234);
		assert_eq!(parse_count("1.2K"), 1200);
		assert_eq!(parse_count("3.4M"), 3_400_000);
		assert_eq!(parse_count("2B"), 2_000_000_000);
		assert_eq!(parse_count("1,234 likes"), 1234);
		assert_eq!(parse_count("5 replies"), 5);
		assert_eq!(parse_count("1 reply"), 1);
	}

	#[test]
	fn unrecognisable_counts_are_zero() {
		assert_eq!(parse_count(""), 0);
		assert_eq!(parse_count("   "), 0);
		assert_eq!(parse_count("K"), 0);
		assert_eq!(parse_count("Reply"), 0);
	}

	#[test]
	fn prefers_the_exact_label_over_the_abbreviated_count() {
		let meta = Meta::from_value(&serde_json::json!({"likes": "1.2K", "likesLabel": "1,234 likes"}));
		assert_eq!(meta.likes, 1234);
		assert_eq!(meta.likes_text.as_deref(), Some("1.2K"));

		// No label, or one that says nothing: fall back to what is rendered.
		let meta = Meta::from_value(&serde_json::json!({"likes": "1.2K", "likesLabel": null}));
		assert_eq!(meta.likes, 1200);
	}

	#[test]
	fn an_unliked_comment_reads_as_zero() {
		let meta = Meta::from_value(&serde_json::json!({"likes": "", "likesLabel": null, "author": "Someone"}));
		assert_eq!(meta.likes, 0);
		assert_eq!(meta.likes_text, None);
		assert_eq!(meta.author.as_deref(), Some("Someone"));
	}

	#[test]
	fn names_captures_after_the_post_they_belong_to() {
		let target = Target {
			index: 0,
			id: "Ugkx/../Abc".to_string(),
		};
		assert_eq!(post_dir_name(&target), "0001_UgkxAbc");
		assert_eq!(comment_file_name(0, Some("UgwXyZ"), ImageFormat::Png), "001_UgwXyZ.png");
		assert_eq!(comment_file_name(11, None, ImageFormat::Webp(80)), "012_unknown.webp");
	}
}
