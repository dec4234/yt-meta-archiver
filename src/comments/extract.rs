//! Driving a video's watch page: load its comments, keep the ones worth
//! keeping, and write an index of them.
//!
//! The like threshold is applied before any screenshot is taken, since on a busy
//! video that filter is the difference between a page of captures and thousands.
//!
//! Threads are read a comment at a time rather than whole: the top-level comment
//! is its own entry, and with `--subcomments` the first few replies under it are
//! each another, nested under it in the index and — when pictures were asked for
//! — screenshotted into a folder named after the comment they answer. A thread is
//! left collapsed unless replies were asked for.

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result};
use log::{debug, info, trace, warn};
use serde_json::Value;

use super::meta::{self, Meta};
use super::replies;
use crate::browser::ImageFormat;
use crate::clip::Clipper;
use crate::profile_picture::ProfilePictures;
use crate::{browser, time, video};

/// Sidecar index written next to the screenshots.
pub(super) const INDEX_FILE: &str = "comments.json";

/// Rounds of scrolling that must find no new comments before we call it done.
const STABLE_ROUNDS: u32 = 3;

/// The same, before the first comment has appeared. A watch page renders its
/// video long before it renders anything below it, so an empty comment section
/// early on means "not yet" far more often than it means "none".
const WARMUP_ROUNDS: u32 = 20;

const MAX_SCROLLS: u32 = 500;

/// How a run is to be carried out, beyond which video it is about.
pub(super) struct Options<'a> {
	/// Keep only comments with at least this many likes.
	pub min_likes: u64,
	/// Stop after this many kept comments.
	pub max: Option<usize>,
	/// Record up to this many replies under each kept comment; zero leaves every
	/// thread collapsed.
	pub subcomments: usize,
	/// Screenshot each kept comment as well as recording it.
	pub pics: bool,
	/// What those screenshots are encoded as.
	pub format: ImageFormat,
	/// Where each author's profile picture goes, when they are wanted.
	pub pictures: Option<&'a ProfilePictures>,
}

/// Reads the video's comments, writes `comments.json` (and, with `pics`, a
/// screenshot per comment) into `out_dir`, and returns how many comments the
/// index ended up with.
pub(super) fn extract(video_id: &str, out_dir: &Path, opts: &Options) -> Result<usize> {
	std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

	let url = video::watch_url(video_id);
	debug!("starting a browser session for {url}");
	let browser = browser::launch()?;
	let tab = browser.new_tab()?;
	tab.set_default_timeout(Duration::from_secs(60));
	browser::emulate_dark(&tab)?;

	tab.navigate_to(&url)?.wait_until_navigated()?;
	browser::set_dark_theme_cookie(&tab)?;
	tab.reload(false, None)?.wait_until_navigated()?;

	let clipper = Clipper::new(&tab, opts.format);
	clipper.init_viewport()?;
	clipper.eval(super::COMMENTS_JS)?;
	clipper.eval("__yta.prepare()")?;
	if opts.subcomments > 0 {
		clipper.eval("__yta.subcomments(true)")?;
	}

	let loaded = load_comments(&clipper, opts)?;
	// Read once the page has settled: straight after the reload the title is
	// still the bare "YouTube" the document starts life with.
	let title = page_title(&clipper);
	if let Some(title) = &title {
		info!("title:   {title}");
	}

	if loaded == 0 {
		if clipper.eval("__yta.disabled()")?.as_bool().unwrap_or(false) {
			info!("this video has comments turned off");
		} else {
			info!("this video has no comments");
		}
		write_index(out_dir, video_id, title.as_deref(), opts, 0, &[])?;
		return Ok(0);
	}

	// What the page ended up with, which the last scroll's count can understate:
	// a round that stops early still leaves whatever it had just rendered.
	let metas: Vec<Meta> = clipper.eval("__yta.metas()")?.as_array().map(|list| list.iter().map(Meta::from_value).collect()).unwrap_or_default();
	let loaded = metas.len();
	info!("loaded {loaded} comments");

	let mut wanted: Vec<usize> = (0..loaded).filter(|&i| metas[i].likes >= opts.min_likes).collect();
	if opts.min_likes > 0 {
		info!("{} of them clear {} likes", wanted.len(), opts.min_likes);
	}
	if let Some(max) = opts.max
		&& wanted.len() > max
	{
		info!("keeping the first {max}");
		wanted.truncate(max);
	}

	let mut entries = collect(&clipper, &metas, &wanted, video_id, out_dir, opts);
	if let Some(pictures) = opts.pictures {
		pictures.attach_to_comments(&mut entries);
	}
	write_index(out_dir, video_id, title.as_deref(), opts, loaded, &entries)?;
	Ok(entries.len())
}

/// Turns the wanted comments into index entries, capturing each one first when
/// `pics` is set, and nesting whatever replies were asked for under it.
fn collect(clipper: &Clipper, metas: &[Meta], wanted: &[usize], video_id: &str, out_dir: &Path, opts: &Options) -> Vec<Value> {
	let mut entries = Vec::with_capacity(wanted.len());
	for (ordinal, &i) in wanted.iter().enumerate() {
		let mut meta = metas[i].clone();
		let mut file = None;
		if opts.pics {
			match capture_comment(clipper, i, ordinal, &meta, out_dir, opts) {
				Ok(Some((name, fresh))) => {
					// The capture expands a clamped body, so its text is the
					// fuller one.
					if fresh.text.is_some() {
						meta.text = fresh.text;
					}
					info!("[{}] {name}", ordinal + 1);
					file = Some(name);
				}
				Ok(None) => warn!("[{}] left the page before it could be captured", ordinal + 1),
				Err(e) => {
					// Losing the browser costs us the remaining captures, so
					// record what is left without pictures rather than throwing
					// away comments we have already read.
					if !clipper.is_alive() {
						warn!("browser session ended on comment {}; the rest are recorded without pictures", ordinal + 1);
						entries.extend(wanted[ordinal..].iter().map(|&i| entry(&metas[i], video_id, None)));
						return entries;
					}
					warn!("[{}] not captured: {e:#}", ordinal + 1);
				}
			}
		}

		let mut value = entry(&meta, video_id, file.as_deref());
		if opts.subcomments > 0 {
			let thread = Thread {
				ordinal,
				index: i,
				parent_id: meta.id.clone(),
			};
			let subs = collect_replies(clipper, &thread, video_id, out_dir, opts);
			if let Some(value) = value.as_object_mut() {
				value.insert("subcomments".to_string(), Value::Array(subs));
			}
			// Same bargain as a lost capture: what is already read is worth more
			// than the rest of the run, which has no browser left to make it in.
			if !clipper.is_alive() {
				warn!("browser session ended on comment {} replies; the rest are recorded without replies or pictures", ordinal + 1);
				entries.push(value);
				entries.extend(wanted[ordinal + 1..].iter().map(|&i| entry(&metas[i], video_id, None)));
				return entries;
			}
		}
		entries.push(value);
	}
	entries
}

/// The top-level comment a batch of replies hangs off.
struct Thread {
	/// Its place among the comments kept for this video, which its screenshot and
	/// the folder of replies under it are both named after.
	ordinal: usize,
	/// Its place among every thread on the page, which is how the page is asked
	/// about it.
	index: usize,
	parent_id: Option<String>,
}

/// Screenshots comment `i`, returning the file name recorded in the index and
/// what the page said about it at capture time, or `None` for a comment the page
/// no longer has.
fn capture_comment(clipper: &Clipper, i: usize, ordinal: usize, meta: &Meta, out_dir: &Path, opts: &Options) -> Result<Option<(String, Meta)>> {
	// With replies in the capture list, opening the threads before this one has
	// moved it along, so where it sits now is asked rather than assumed.
	let node = if opts.subcomments > 0 {
		match replies::select(clipper, i, None)? {
			Some(node) => node,
			None => return Ok(None),
		}
	} else {
		i
	};
	let file = file_name(ordinal, meta.id.as_deref(), opts.format);
	let captured = capture_node(clipper, node, &file, out_dir)?;
	Ok(Some((file, captured)))
}

/// Opens `thread`'s replies and records up to `--subcomments` of them, in the
/// order the page shows them, screenshotting each into a folder of its own under
/// the parent comment's name when pictures were asked for.
///
/// Best effort throughout: a thread that will not open, or a reply that will not
/// capture, costs only itself — the comment it hangs off is already indexed. A
/// caller that finds the browser gone afterwards should stop.
fn collect_replies(clipper: &Clipper, thread: &Thread, video_id: &str, out_dir: &Path, opts: &Options) -> Vec<Value> {
	let (ordinal, i, limit) = (thread.ordinal, thread.index, opts.subcomments);
	let loaded = match replies::load(clipper, i, limit) {
		Ok(0) => return Vec::new(),
		Ok(loaded) => loaded,
		Err(e) => {
			warn!("[{}] replies did not open: {e:#}", ordinal + 1);
			return Vec::new();
		}
	};

	let metas = match replies::metas(clipper, i) {
		Ok(metas) => metas,
		Err(e) => {
			warn!("[{}] replies could not be read: {e:#}", ordinal + 1);
			return Vec::new();
		}
	};
	let keep = metas.len().min(limit);
	debug!("[{}] {loaded} replies loaded, keeping {keep}", ordinal + 1);
	if keep == 0 {
		return Vec::new();
	}

	// Made only once there is something to put in it, so a thread whose replies
	// all fell through does not leave an empty folder behind. A metadata-only run
	// needs no folder at all.
	let dir_name = replies::dir_name(ordinal, thread.parent_id.as_deref());
	let dir = out_dir.join(&dir_name);
	if opts.pics
		&& let Err(e) = std::fs::create_dir_all(&dir)
	{
		warn!("[{}] replies have nowhere to go: {e:#}", ordinal + 1);
		return Vec::new();
	}

	let mut entries = Vec::with_capacity(keep);
	for (r, meta) in metas.iter().enumerate().take(keep) {
		let mut meta = meta.clone();
		let mut file = None;
		if opts.pics {
			match capture_reply(clipper, i, r, &meta, &dir, opts.format) {
				Ok(Some((captured, fresh))) => {
					if fresh.text.is_some() {
						meta.text = fresh.text;
					}
					// Recorded the way a comment's own screenshot is: a path
					// relative to the output directory.
					let relative = format!("{dir_name}/{captured}");
					info!("[{}.{}] {relative}", ordinal + 1, r + 1);
					file = Some(relative);
				}
				Ok(None) => warn!("[{}.{}] reply left the page", ordinal + 1, r + 1),
				Err(e) => {
					if !clipper.is_alive() {
						entries.push(entry(&meta, video_id, None));
						return entries;
					}
					warn!("[{}.{}] reply not captured: {e:#}", ordinal + 1, r + 1);
				}
			}
		}
		entries.push(entry(&meta, video_id, file.as_deref()));
	}
	entries
}

/// Screenshots thread `i`'s `r`th reply into `dir`, returning the file name it
/// was written under and what the page said about it at capture time.
fn capture_reply(clipper: &Clipper, i: usize, r: usize, meta: &Meta, dir: &Path, format: ImageFormat) -> Result<Option<(String, Meta)>> {
	let Some(node) = replies::select(clipper, i, Some(r))? else {
		return Ok(None);
	};
	// Numbered within the thread it belongs to rather than across the video, since
	// it lives in a folder of that thread's own.
	let file = file_name(r, meta.id.as_deref(), format);
	let captured = capture_node(clipper, node, &file, dir)?;
	Ok(Some((file, captured)))
}

/// Screenshots the element at `node` into `dir`, returning what the page said
/// about it at capture time.
fn capture_node(clipper: &Clipper, node: usize, file: &str, dir: &Path) -> Result<Meta> {
	let (image, info) = clipper.capture(node)?;
	let path = dir.join(file);
	std::fs::write(&path, image).with_context(|| format!("writing {}", path.display()))?;
	Ok(Meta::from_value(&info["meta"]))
}

/// Scrolls the watch page until every top-level comment has loaded, or until
/// `max` of them already clear the like threshold. Returns how many are on the
/// page.
fn load_comments(clipper: &Clipper, opts: &Options) -> Result<usize> {
	let count = |clipper: &Clipper| -> Result<usize> { Ok(clipper.eval("__yta.count()")?.as_u64().unwrap_or(0) as usize) };

	let mut last = count(clipper)?;
	let mut stable = 0;
	for _ in 0..MAX_SCROLLS {
		if enough(clipper, opts, last)? {
			debug!("stopping early with {last} comments loaded");
			break;
		}
		let scrolled = clipper.eval("__yta.scrollToBottom()")?.as_u64().unwrap_or(0) as usize;
		sleep(Duration::from_millis(700));
		let now = scrolled.max(count(clipper)?);

		if now > last {
			trace!("loaded {now} comments...");
			last = now;
			stable = 0;
		} else {
			stable += 1;
			let allowed = if last == 0 {
				WARMUP_ROUNDS
			} else {
				STABLE_ROUNDS
			};
			trace!("no new comments ({stable}/{allowed} stable rounds)");
			if stable >= allowed {
				break;
			}
		}
	}
	debug!("scrolling done with {last} comments on the page");
	clipper.eval("window.scrollTo(0, 0)")?;
	Ok(last)
}

/// Whether `--max` is already satisfied by what is on the page. Without a like
/// threshold that is just the count; with one, the likes have to be read, which
/// is why they are fetched on their own rather than as whole metas.
fn enough(clipper: &Clipper, opts: &Options, loaded: usize) -> Result<bool> {
	let Some(max) = opts.max else {
		return Ok(false);
	};
	if opts.min_likes == 0 {
		return Ok(loaded >= max);
	}
	if loaded < max {
		return Ok(false); // cannot be enough qualifying ones yet
	}
	let qualifying = clipper
		.eval("__yta.likeCounts()")?
		.as_array()
		.map(|list| list.iter().filter(|v| Meta::from_value(v).likes >= opts.min_likes).count())
		.unwrap_or(0);
	Ok(qualifying >= max)
}

/// The video's title, as the page has it. Best effort: it is a nicety in the
/// index, not something to fail a run over.
fn page_title(clipper: &Clipper) -> Option<String> {
	let title = clipper.eval("document.title").ok()?;
	strip_site_name(title.as_str()?)
}

/// The video's half of a `<title>`, which is `<video> - YouTube`. A page that
/// has not named its video yet says only "YouTube", which is nobody's title.
fn strip_site_name(title: &str) -> Option<String> {
	let title = title.trim();
	let title = title.strip_suffix("- YouTube").unwrap_or(title).trim();
	(!title.is_empty() && title != "YouTube").then(|| title.to_string())
}

/// One comment's index entry, pointing at the comment on the watch page.
fn entry(meta: &Meta, video_id: &str, screenshot: Option<&str>) -> Value {
	let url = meta.id.as_ref().map(|id| format!("{}&lc={id}", video::watch_url(video_id)));
	meta::entry(meta, url, screenshot)
}

fn write_index(out_dir: &Path, video_id: &str, title: Option<&str>, opts: &Options, loaded: usize, entries: &[Value]) -> Result<()> {
	let path = out_dir.join(INDEX_FILE);
	let doc = serde_json::json!({
		"video": video::watch_url(video_id),
		"title": title,
		"captured_at": time::now_utc(),
		// What the list was filtered by, so a short index is not mistaken for a
		// video that was barely commented on.
		"min_likes": opts.min_likes,
		"max": opts.max,
		// How many replies were kept under each comment, so a thread that reads as
		// short in the index is not mistaken for one that was barely replied to.
		"subcomments": opts.subcomments,
		"comments_loaded": loaded,
		"comments": entries,
	});
	debug!("writing index with {} entries to {}", entries.len(), path.display());
	std::fs::write(&path, serde_json::to_string_pretty(&doc)?).with_context(|| format!("writing {}", path.display()))
}

/// `001_UgwXyZ.png`, in the order the page showed the comments.
fn file_name(ordinal: usize, id: Option<&str>, format: ImageFormat) -> String {
	format!("{:03}_{}.{}", ordinal + 1, meta::sanitize(id), format.extension())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn takes_the_video_out_of_the_page_title() {
		assert_eq!(strip_site_name("Never Gonna Give You Up - YouTube").as_deref(), Some("Never Gonna Give You Up"));
		assert_eq!(strip_site_name("  A video  ").as_deref(), Some("A video"));
		// A page that has not named its video yet.
		assert_eq!(strip_site_name("YouTube"), None);
		assert_eq!(strip_site_name(""), None);
	}

	#[test]
	fn names_captures_in_the_order_the_page_showed_them() {
		assert_eq!(file_name(0, Some("UgwXyZ"), ImageFormat::Png), "001_UgwXyZ.png");
		assert_eq!(file_name(11, None, ImageFormat::Webp(80)), "012_unknown.webp");
	}

	#[test]
	fn points_an_entry_at_the_comment_on_the_watch_page() {
		let meta = Meta::from_value(&serde_json::json!({"id": "UgwXyZ", "author": "Someone", "likes": "12"}));
		let captured = entry(&meta, "dQw4w9WgXcQ", Some("001_UgwXyZ.png"));
		assert_eq!(captured["url"], "https://www.youtube.com/watch?v=dQw4w9WgXcQ&lc=UgwXyZ");
		assert_eq!(captured["likes"], 12);
		assert_eq!(captured["screenshot"], "001_UgwXyZ.png");

		// No id to link to, and no picture taken: both say so rather than
		// pointing at something that is not there.
		let bare = entry(&Meta::default(), "dQw4w9WgXcQ", None);
		assert!(bare["url"].is_null());
		assert!(bare["screenshot"].is_null());
	}
}
