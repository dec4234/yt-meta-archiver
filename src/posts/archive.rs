//! Driving the posts tab: load every post, then clip a screenshot of each.
//!
//! A run is a series of batches, each in its own browser session, whose
//! captures are folded into the `posts.json` index.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use headless_chrome::Tab;
use headless_chrome::protocol::cdp::Page;
use log::{debug, info, trace, warn};
use serde_json::Value;

use super::details::{self, Details};
use crate::browser::ImageFormat;
use crate::{browser, time};

const ARCHIVER_JS: &str = include_str!("js/archiver.js");

/// Tallest viewport we will grow to for one oversized post. Chromium gets
/// unhappy allocating capture surfaces much beyond this.
const MAX_CAPTURE_HEIGHT: u32 = 12000;

/// Viewport space kept free around a post: enough for the margin `align`
/// leaves above it, plus slack below.
const HEADROOM: u32 = 400;

/// Rounds of scrolling that must find no new posts before we call it done.
const STABLE_ROUNDS: u32 = 3;
const MAX_SCROLLS: u32 = 500;

/// Posts captured per browser session. A long feed plus a few dozen large
/// captures is enough to exhaust the renderer, so each batch starts fresh.
const BATCH: usize = 30;

/// Sidecar index written next to the screenshots.
pub(super) const INDEX_FILE: &str = "posts.json";

/// One screenshot that was written, before its exact date is looked up.
struct Capture {
	/// Position in the feed, for log messages.
	index: usize,
	file: String,
	/// The post's YouTube id, absent when the page did not expose one.
	id: Option<String>,
	/// The relative time YouTube shows, e.g. "2 years ago".
	relative: Value,
}

/// Where a batch stopped, so the next session knows where to resume.
struct Progress {
	/// First post the next batch should capture.
	next: usize,
	/// The feed had no more posts to load.
	end_of_feed: bool,
	/// One entry per screenshot written, in capture order.
	captures: Vec<Capture>,
}

/// Captures every post on the channel, restarting the browser between batches
/// (and after a crash) so a long feed does not outlive the renderer. Writes
/// `posts.json` alongside the screenshots and returns how many were written.
///
/// `fetch_details` turns on the per-post lookup of the exact date and post
/// text, which costs one page fetch per post. `workers` batches are captured
/// side by side, each in its own browser session; 1 keeps everything on this
/// thread. `format` is what the screenshots are encoded as.
pub(super) fn archive(posts_url: &str, out_dir: &Path, limit: Option<usize>, fetch_details: bool, workers: usize, format: ImageFormat) -> Result<usize> {
	std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

	let mut next = 0;
	// Keys are zero-padded, so the map serialises in capture order.
	let mut index = serde_json::Map::new();

	loop {
		let round = if workers > 1 {
			run_round(posts_url, out_dir, limit, next, workers, format)?
		} else {
			run_session(posts_url, out_dir, limit, next, format)?
		};

		index.extend(resolve_details(&round.captures, fetch_details, workers));
		// Rewritten after every round, so an interrupted run still leaves a
		// usable index for the screenshots on disk.
		write_index(out_dir, posts_url, &index)?;

		if round.end_of_feed || limit.is_some_and(|l| round.next >= l) {
			return Ok(index.len());
		}
		if round.next == next {
			// The session died without capturing anything; skip the post it
			// choked on rather than restarting on it forever.
			warn!("no progress on post {}, skipping it", next + 1);
			next += 1;
		} else {
			next = round.next;
		}
	}
}

/// One browser session, capturing the batch that starts at `start`.
fn run_session(posts_url: &str, out_dir: &Path, limit: Option<usize>, start: usize, format: ImageFormat) -> Result<Progress> {
	debug!("starting a browser session at post {}", start + 1);
	let browser = browser::launch()?;
	let tab = browser.new_tab()?;
	tab.set_default_timeout(Duration::from_secs(60));
	Archiver::new(&tab, out_dir, limit, format).run_batch(posts_url, start)
}

/// Runs up to `workers` browser sessions at once, on consecutive batches
/// starting at `start`, and folds them back into a single [`Progress`].
///
/// Every session loads the feed from the top, so a later batch pays for
/// scrolling past the earlier ones; the capture work, which dominates, is what
/// actually runs in parallel.
fn run_round(posts_url: &str, out_dir: &Path, limit: Option<usize>, start: usize, workers: usize, format: ImageFormat) -> Result<Progress> {
	let starts: Vec<usize> = (0..workers).map(|w| start + w * BATCH).take_while(|s| limit.is_none_or(|l| *s < l)).collect();
	if starts.len() < 2 {
		return run_session(posts_url, out_dir, limit, start, format);
	}
	info!("capturing posts {}+ across {} browser sessions", start + 1, starts.len());

	let results: Vec<Result<Progress>> = std::thread::scope(|scope| {
		let handles: Vec<_> = starts.iter().map(|&s| scope.spawn(move || run_session(posts_url, out_dir, limit, s, format))).collect();
		handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("capture thread panicked")))).collect()
	});

	// Screenshots from every session are kept — they are on disk either way —
	// but the resume point may only advance across an unbroken run of them, so
	// a session that died mid-batch is picked up again next round.
	let mut round = Progress {
		next: start,
		end_of_feed: false,
		captures: Vec::new(),
	};
	let mut broken = false;
	for (&s, result) in starts.iter().zip(results) {
		let wanted = limit.map_or(s + BATCH, |l| (s + BATCH).min(l));
		match result {
			Ok(session) => {
				round.captures.extend(session.captures);
				if broken {
					continue;
				}
				round.next = session.next;
				round.end_of_feed = session.end_of_feed;
				if session.end_of_feed {
					broken = true; // nothing beyond this session to advance past
				} else if session.next < wanted {
					warn!("session at post {} stopped at {}", s + 1, session.next + 1);
					broken = true;
				}
			}
			Err(e) => {
				// `next` is still the previous session's stopping point, which
				// for contiguous batches is exactly this one's start.
				warn!("session at post {} failed: {e:#}", s + 1);
				broken = true;
			}
		}
	}
	Ok(round)
}

/// Turns captures into index entries, looking up each post's exact publication
/// date and text when `fetch_details` is set. The lookups are independent page
/// fetches, so `workers` of them run at a time.
fn resolve_details(captures: &[Capture], fetch_details: bool, workers: usize) -> Vec<(String, Value)> {
	let details: Vec<Option<Details>> = if !fetch_details {
		vec![None; captures.len()]
	} else if workers <= 1 || captures.len() < 2 {
		captures.iter().map(fetch_details_for).collect()
	} else {
		// A shared cursor rather than fixed slices, so one slow post does not
		// leave the other threads idle at the end.
		let cursor = AtomicUsize::new(0);
		let details = Mutex::new(vec![None; captures.len()]);
		std::thread::scope(|scope| {
			for _ in 0..workers.min(captures.len()) {
				scope.spawn(|| {
					loop {
						let i = cursor.fetch_add(1, Ordering::Relaxed);
						let Some(capture) = captures.get(i) else { break };
						let found = fetch_details_for(capture);
						details.lock().expect("details lock poisoned")[i] = found;
					}
				});
			}
		});
		details.into_inner().expect("details lock poisoned")
	};

	captures
		.iter()
		.zip(details)
		.map(|(c, details)| (c.file.clone(), index_entry(c.id.as_deref(), &c.relative, details)))
		.collect()
}

/// Best effort: a failed lookup should not cost us the screenshot. `None` only
/// when the post carried no id to look up; a lookup that failed comes back
/// empty, so the index records that it was tried and found nothing.
fn fetch_details_for(capture: &Capture) -> Option<Details> {
	let id = capture.id.as_deref()?;
	Some(details::fetch(id).unwrap_or_else(|e| {
		warn!("no details for post {}: {e:#}", capture.index + 1);
		Details::default()
	}))
}

fn write_index(out_dir: &Path, posts_url: &str, index: &serde_json::Map<String, Value>) -> Result<()> {
	let path = out_dir.join(INDEX_FILE);
	let doc = serde_json::json!({
		"channel": posts_url,
		"captured_at": time::now_utc(),
		"posts": index,
	});
	debug!("writing index with {} entries to {}", index.len(), path.display());
	std::fs::write(&path, serde_json::to_string_pretty(&doc)?).with_context(|| format!("writing {}", path.display()))
}

struct Archiver<'a> {
	tab: &'a Tab,
	out_dir: PathBuf,
	/// Stop after this many posts; used to sample a channel quickly.
	limit: Option<usize>,
	/// What the screenshots are encoded as.
	format: ImageFormat,
	/// Current layout viewport height, to avoid redundant reflows.
	viewport_height: Cell<u32>,
}

impl<'a> Archiver<'a> {
	fn new(tab: &'a Tab, out_dir: impl Into<PathBuf>, limit: Option<usize>, format: ImageFormat) -> Self {
		Self {
			tab,
			out_dir: out_dir.into(),
			limit,
			format,
			viewport_height: Cell::new(0),
		}
	}

	/// Loads the posts tab in dark theme and captures up to `BATCH` posts
	/// starting at `start`.
	fn run_batch(&self, posts_url: &str, start: usize) -> Result<Progress> {
		browser::emulate_dark(self.tab)?;
		self.init_viewport()?;

		debug!("loading {posts_url}");
		self.tab.navigate_to(posts_url)?.wait_until_navigated()?;
		browser::set_dark_theme_cookie(self.tab)?;
		self.tab.reload(false, None)?.wait_until_navigated()?;
		self.inject()?;

		// Only load as far as this batch needs; the shorter the feed, the less
		// work the renderer is doing while capturing.
		let mut wanted = start + BATCH;
		if let Some(limit) = self.limit {
			wanted = wanted.min(limit);
		}
		let (loaded, end_of_feed) = self.load_posts(wanted)?;
		if loaded == 0 {
			bail!("no community posts found at {posts_url}");
		}
		let end = wanted.min(loaded);
		if start >= end {
			return Ok(Progress {
				next: start,
				end_of_feed: true,
				captures: Vec::new(),
			});
		}

		let mut progress = Progress {
			next: end,
			end_of_feed,
			captures: Vec::new(),
		};
		for i in start..end {
			match self.capture_post(i) {
				Ok(capture) => {
					info!("[{}] {}", i + 1, capture.file);
					progress.captures.push(capture);
				}
				Err(e) => {
					// A dead connection means the browser is gone: hand back to
					// the caller so it can restart and resume here.
					if !self.is_alive() {
						warn!("browser session ended at post {}, restarting", i + 1);
						return Ok(Progress {
							next: i,
							end_of_feed: false,
							..progress
						});
					}
					warn!("[{}] skipped: {e:#}", i + 1);
				}
			}
		}
		Ok(progress)
	}

	/// Whether the page is still reachable, i.e. the browser has not crashed.
	fn is_alive(&self) -> bool {
		self.eval("1").is_ok()
	}

	fn inject(&self) -> Result<()> {
		self.eval(ARCHIVER_JS)?;
		self.eval("__yta.prepare()")?;
		Ok(())
	}

	/// Pins the layout viewport, which otherwise depends on the window's
	/// chrome, so post widths are identical across runs.
	fn init_viewport(&self) -> Result<()> {
		self.set_viewport_height(browser::VIEWPORT.1)
	}

	fn eval(&self, expr: &str) -> Result<Value> {
		browser::eval(self.tab, expr)
	}

	fn count(&self) -> Result<usize> {
		Ok(self.eval("__yta.count()")?.as_u64().unwrap_or(0) as usize)
	}

	/// Scrolls the infinite feed until `wanted` posts are loaded or it runs
	/// out. Returns how many loaded and whether the feed ended.
	fn load_posts(&self, wanted: usize) -> Result<(usize, bool)> {
		let mut last = self.count()?;
		let mut stable = 0;
		let mut end_of_feed = false;

		for _ in 0..MAX_SCROLLS {
			if last >= wanted {
				break;
			}
			let count = self.eval("__yta.scrollToBottom()")?.as_u64().unwrap_or(0) as usize;
			sleep(Duration::from_millis(700));
			let count = count.max(self.count()?);

			if count > last {
				debug!("loaded {count} posts...");
				last = count;
				stable = 0;
			} else {
				stable += 1;
				trace!("no new posts ({stable}/{STABLE_ROUNDS} stable rounds)");
				if stable >= STABLE_ROUNDS {
					end_of_feed = true;
					break;
				}
			}
		}
		info!("loaded {last} posts");
		self.eval("window.scrollTo(0, 0)")?;
		Ok((last, end_of_feed))
	}

	/// Brings post `i` into view and waits until its images have decoded, so a
	/// post with an attachment is never captured half-blank.
	fn focus_until_loaded(&self, i: usize) -> Result<Value> {
		let deadline = Instant::now() + Duration::from_secs(20);
		loop {
			let info = self.eval(&format!("__yta.focus({i})"))?;
			if !info["exists"].as_bool().unwrap_or(false) {
				bail!("post {i} disappeared from the page");
			}
			if info["ready"].as_bool().unwrap_or(false) {
				// A beat for expanded text and just-decoded images to paint,
				// then measure the settled layout.
				sleep(Duration::from_millis(400));
				return self.eval(&format!("__yta.focus({i})"));
			}
			if Instant::now() > deadline {
				warn!("post {} images still loading, capturing anyway", i + 1);
				return Ok(info);
			}
			sleep(Duration::from_millis(250));
		}
	}

	/// Resizes the layout viewport, keeping the width fixed so YouTube's
	/// desktop layout and column widths do not change between posts. Resizing
	/// reflows the feed, so this is a no-op when the height already matches.
	fn set_viewport_height(&self, height: u32) -> Result<()> {
		if self.viewport_height.get() == height {
			return Ok(());
		}
		browser::set_viewport(self.tab, browser::VIEWPORT.0, height)?;
		trace!("viewport height {} -> {height}", self.viewport_height.get());
		self.viewport_height.set(height);
		sleep(Duration::from_millis(250)); // let the reflow settle
		Ok(())
	}

	/// Sizes the viewport so a post of `height` fits with room for the margin
	/// `align` leaves above it.
	fn fit_viewport(&self, height: f64) -> Result<()> {
		let needed = height.ceil() as u32 + HEADROOM;
		self.set_viewport_height(needed.max(browser::VIEWPORT.1).min(MAX_CAPTURE_HEIGHT))
	}

	fn align(&self, i: usize) -> Result<Value> {
		let info = self.eval(&format!("__yta.align({i})"))?;
		if !info["exists"].as_bool().unwrap_or(false) {
			bail!("post {i} disappeared from the page");
		}
		Ok(info)
	}

	/// Scrolls post `i` into view, waits for its images, and writes the clip.
	///
	/// The post is captured entirely within the viewport — the viewport is
	/// grown to fit a tall post rather than relying on `captureBeyondViewport`,
	/// which reflows YouTube's lazily rendered feed and misaligns the clip.
	/// Returns the screenshot's file name and the metadata its index entry
	/// needs; the exact-date lookup happens later, off the browser session.
	fn capture_post(&self, i: usize) -> Result<Capture> {
		let info = self.focus_until_loaded(i)?;

		let height = info["rect"]["height"].as_f64().unwrap_or(0.0);
		if height <= 0.0 {
			bail!("post has zero height");
		}
		if height.ceil() as u32 + HEADROOM > MAX_CAPTURE_HEIGHT {
			bail!("post is {height:.0}px tall, beyond the capture limit");
		}
		self.fit_viewport(height)?;

		// Re-measure after the resize, immediately before capturing. If the
		// post still runs off the bottom — near the end of the feed there may
		// not be enough page left to scroll — grow to exactly what it needs.
		let mut info = self.align(i)?;
		let overflow = info["viewTop"].as_f64().unwrap_or(0.0) + height - info["viewHeight"].as_f64().unwrap_or(0.0);
		if overflow > 0.0 {
			debug!("post {} overflows the viewport by {overflow:.0}px, growing", i + 1);
			self.fit_viewport(height + overflow)?;
			info = self.align(i)?;
		}

		let rect = &info["rect"];
		let clip = Page::Viewport {
			x: rect["x"].as_f64().unwrap_or(0.0),
			y: rect["y"].as_f64().unwrap_or(0.0),
			width: rect["width"].as_f64().unwrap_or(0.0),
			height: rect["height"].as_f64().unwrap_or(0.0),
			scale: 2.0, // capture at 2x for legible text
		};

		let image = browser::capture(self.tab, clip, self.format)?;
		debug!(
			"post {} captured, {:.0}x{:.0}px, {} KiB",
			i + 1,
			rect["width"].as_f64().unwrap_or(0.0),
			rect["height"].as_f64().unwrap_or(0.0),
			image.len() / 1024
		);

		let meta = &info["meta"];
		let id = meta["id"].as_str();
		let file = file_name(i, id, self.format);
		let path = self.out_dir.join(&file);
		std::fs::write(&path, image).with_context(|| format!("writing {}", path.display()))?;

		Ok(Capture {
			index: i,
			file,
			id: id.map(str::to_string),
			relative: meta["published"].clone(),
		})
	}
}

/// The `posts.json` value for one screenshot: where the post lives, its text,
/// when it went up, and the relative time YouTube shows in the screenshot
/// itself.
///
/// `posted_utc` and `text` are absent entirely when the lookup was not
/// requested, and null when it was made but came back without them — the two
/// are worth telling apart when reading an index back.
fn index_entry(id: Option<&str>, relative: &Value, details: Option<Details>) -> Value {
	let mut entry = serde_json::Map::new();
	entry.insert("url".to_string(), id.map(super::post_url).into());
	if let Some(details) = details {
		entry.insert("posted_utc".to_string(), details.published_utc.into());
		entry.insert("text".to_string(), details.text.into());
	}
	entry.insert("posted_relative".to_string(), relative.clone());
	Value::Object(entry)
}

/// `0001_UgkxAbC.png`, keeping the channel's newest-first ordering sortable.
fn file_name(i: usize, id: Option<&str>, format: ImageFormat) -> String {
	let id = id
		.map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
		.filter(|s| !s.is_empty())
		.unwrap_or_else(|| "unknown".to_string());
	format!("{:04}_{}.{}", i + 1, id, format.extension())
}
