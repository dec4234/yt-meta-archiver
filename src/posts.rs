//! Driving the posts tab: load every post, then clip a screenshot of each.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use headless_chrome::Tab;
use log::{debug, info, trace, warn};
use headless_chrome::protocol::cdp::{Emulation, Page, Runtime};
use serde_json::Value;

use crate::{browser, published};

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
pub const INDEX_FILE: &str = "posts.json";

/// Where a batch stopped, so the next session knows where to resume.
struct Progress {
	/// First post the next batch should capture.
	next: usize,
	/// The feed had no more posts to load.
	end_of_feed: bool,
	/// One entry per screenshot written, keyed by file name.
	entries: Vec<(String, Value)>,
}

/// Captures every post on the channel, restarting the browser between batches
/// (and after a crash) so a long feed does not outlive the renderer. Writes
/// `posts.json` alongside the screenshots and returns how many were written.
///
/// `fetch_dates` turns on the exact-date lookup, which costs one page fetch
/// per post.
pub fn archive(posts_url: &str, out_dir: &Path, limit: Option<usize>, fetch_dates: bool) -> Result<usize> {
	std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

	let mut next = 0;
	// Keys are zero-padded, so the map serialises in capture order.
	let mut index = serde_json::Map::new();

	loop {
		debug!("starting a browser session at post {}", next + 1);
		let browser = browser::launch()?;
		let tab = browser.new_tab()?;
		tab.set_default_timeout(Duration::from_secs(60));

		let progress = Archiver::new(&tab, out_dir, limit, fetch_dates).run_batch(posts_url, next)?;
		index.extend(progress.entries);
		// Rewritten after every batch, so an interrupted run still leaves a
		// usable index for the screenshots on disk.
		write_index(out_dir, posts_url, &index)?;

		if progress.end_of_feed || limit.is_some_and(|l| progress.next >= l) {
			return Ok(index.len());
		}
		if progress.next == next {
			// The session died without capturing anything; skip the post it
			// choked on rather than restarting on it forever.
			warn!("no progress on post {}, skipping it", next + 1);
			next += 1;
		} else {
			next = progress.next;
		}
	}
}

fn write_index(out_dir: &Path, posts_url: &str, index: &serde_json::Map<String, Value>) -> Result<()> {
	let path = out_dir.join(INDEX_FILE);
	let doc = serde_json::json!({
		"channel": posts_url,
		"captured_at": published::now_utc(),
		"posts": index,
	});
	debug!("writing index with {} entries to {}", index.len(), path.display());
	std::fs::write(&path, serde_json::to_string_pretty(&doc)?).with_context(|| format!("writing {}", path.display()))
}

pub struct Archiver<'a> {
	tab: &'a Tab,
	out_dir: PathBuf,
	/// Stop after this many posts; used to sample a channel quickly.
	limit: Option<usize>,
	/// Look up each post's exact publication date.
	fetch_dates: bool,
	/// Current layout viewport height, to avoid redundant reflows.
	viewport_height: Cell<u32>,
}

impl<'a> Archiver<'a> {
	fn new(tab: &'a Tab, out_dir: impl Into<PathBuf>, limit: Option<usize>, fetch_dates: bool) -> Self {
		Self {
			tab,
			out_dir: out_dir.into(),
			limit,
			fetch_dates,
			viewport_height: Cell::new(0),
		}
	}

	/// Loads the posts tab in dark theme and captures up to `BATCH` posts
	/// starting at `start`.
	fn run_batch(&self, posts_url: &str, start: usize) -> Result<Progress> {
		self.emulate_dark()?;
		self.init_viewport()?;

		debug!("loading {posts_url}");
		self.tab.navigate_to(posts_url)?.wait_until_navigated()?;
		// YouTube renders the theme server-side from this preference, so set it
		// and reload rather than fighting a light-theme first paint.
		self.eval("document.cookie = 'PREF=f6=400; domain=.youtube.com; path=/'")?;
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
				entries: Vec::new(),
			});
		}

		let mut progress = Progress {
			next: end,
			end_of_feed,
			entries: Vec::new(),
		};
		for i in start..end {
			match self.capture_post(i) {
				Ok((file, entry)) => {
					info!("[{}] {file}", i + 1);
					progress.entries.push((file, entry));
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

	/// Makes the page report a dark colour-scheme preference, which is what
	/// YouTube's "use device theme" default follows.
	fn emulate_dark(&self) -> Result<()> {
		self.tab.call_method(Emulation::SetEmulatedMedia {
			media: Some("screen".to_string()),
			features: Some(vec![Emulation::MediaFeature {
				name: "prefers-color-scheme".to_string(),
				value: "dark".to_string(),
			}]),
		})?;
		Ok(())
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

	/// Evaluates `expr` in the page and returns its value, reporting any
	/// JavaScript exception rather than silently yielding nothing.
	fn eval(&self, expr: &str) -> Result<Value> {
		let result = self.tab.call_method(Runtime::Evaluate {
			expression: expr.to_string(),
			return_by_value: Some(true),
			await_promise: Some(true),
			user_gesture: Some(true),
			silent: Some(false),
			generate_preview: Some(false),
			include_command_line_api: Some(false),
			object_group: None,
			context_id: None,
			throw_on_side_effect: None,
			timeout: None,
			disable_breaks: None,
			repl_mode: None,
			allow_unsafe_eval_blocked_by_csp: None,
			unique_context_id: None,
			serialization_options: None,
		})?;

		if let Some(details) = result.exception_details {
			let message = details.exception.and_then(|e| e.description).unwrap_or(details.text);
			bail!("JS error in `{}`: {message}", expr.lines().next().unwrap_or(expr));
		}
		Ok(result.result.value.unwrap_or(Value::Null))
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
		self.tab
			.call_method(Emulation::SetDeviceMetricsOverride {
				width: browser::VIEWPORT.0,
				height,
				device_scale_factor: 1.0,
				mobile: false,
				scale: None,
				screen_width: None,
				screen_height: None,
				position_x: None,
				position_y: None,
				dont_set_visible_size: None,
				screen_orientation: None,
				viewport: None,
				display_feature: None,
				device_posture: None,
			})
			.context("resizing viewport")?;
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
	/// Returns the screenshot's file name and its index entry.
	fn capture_post(&self, i: usize) -> Result<(String, Value)> {
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

		let data = self
			.tab
			.call_method(Page::CaptureScreenshot {
				format: Some(Page::CaptureScreenshotFormatOption::Png),
				clip: Some(clip),
				quality: None,
				from_surface: Some(true),
				capture_beyond_viewport: Some(false),
				optimize_for_speed: None,
			})?
			.data;
		let png = base64::prelude::BASE64_STANDARD.decode(data).context("decoding screenshot")?;
		debug!(
			"post {} captured, {:.0}x{:.0}px, {} KiB",
			i + 1,
			rect["width"].as_f64().unwrap_or(0.0),
			rect["height"].as_f64().unwrap_or(0.0),
			png.len() / 1024
		);

		let meta = &info["meta"];
		let id = meta["id"].as_str();
		let file = file_name(i, id);
		let path = self.out_dir.join(&file);
		std::fs::write(&path, png).with_context(|| format!("writing {}", path.display()))?;

		// Best effort: a missing date should not cost us the screenshot.
		let posted_utc = id.filter(|_| self.fetch_dates).map(|id| match published::fetch_utc(id) {
			Ok(date) => Value::String(date),
			Err(e) => {
				warn!("no exact date for post {}: {e:#}", i + 1);
				Value::Null
			}
		});
		Ok((file, index_entry(id, &meta["published"], posted_utc)))
	}
}

/// The `posts.json` value for one screenshot: where the post lives, when it
/// went up, and the relative time YouTube shows in the screenshot itself.
///
/// `posted_utc` is absent entirely when exact dates were not requested, and
/// null when the lookup was attempted but failed — the two are worth telling
/// apart when reading an index back.
fn index_entry(id: Option<&str>, relative: &Value, posted_utc: Option<Value>) -> Value {
	let mut entry = serde_json::Map::new();
	entry.insert("url".to_string(), id.map(|id| format!("https://www.youtube.com/post/{id}")).into());
	if let Some(posted_utc) = posted_utc {
		entry.insert("posted_utc".to_string(), posted_utc);
	}
	entry.insert("posted_relative".to_string(), relative.clone());
	Value::Object(entry)
}

/// `0001_UgkxAbC.png`, keeping the channel's newest-first ordering sortable.
fn file_name(i: usize, id: Option<&str>) -> String {
	let id = id
		.map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
		.filter(|s| !s.is_empty())
		.unwrap_or_else(|| "unknown".to_string());
	format!("{:04}_{}.png", i + 1, id)
}

/// `https://www.youtube.com/@handle/posts` from a handle, URL, or posts URL.
pub fn posts_url(input: &str) -> String {
	let input = input.trim().trim_end_matches('/');
	if input.starts_with("http") {
		if input.ends_with("/posts") {
			input.to_string()
		} else {
			format!("{input}/posts")
		}
	} else {
		let handle = input.strip_prefix('@').unwrap_or(input);
		format!("https://www.youtube.com/@{handle}/posts")
	}
}

/// Directory name derived from the channel handle, e.g. `screenshots/DanielLarson-ob2jw`.
pub fn out_dir_for(posts_url: &str, base: &Path) -> PathBuf {
	let handle = posts_url.split('/').find(|s| s.starts_with('@')).map(|s| s.trim_start_matches('@')).unwrap_or("channel");
	base.join(handle)
}
