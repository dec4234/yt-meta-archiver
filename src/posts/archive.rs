//! Driving the posts tab: load every post, then clip a screenshot of each.
//!
//! A run is a series of batches, each in its own browser session, whose
//! captures are folded into the `posts.json` index.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use headless_chrome::Tab;
use log::{debug, info, trace, warn};
use serde_json::Value;

use super::comments::{self, Target};
use super::details::{self, Details};
use crate::browser::ImageFormat;
use crate::clip::Clipper;
use crate::{browser, time};

const ARCHIVER_JS: &str = include_str!("js/archiver.js");

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

/// How a run is to be carried out, beyond where its output goes.
pub(super) struct Options {
	/// Stop after this many posts; used to sample a channel quickly.
	pub limit: Option<usize>,
	/// Look up each post's exact date and text, which costs a page fetch per
	/// post.
	pub details: bool,
	/// Capture each post's top-level comments too, keeping those with at least
	/// this many likes.
	pub comments: Option<u64>,
	/// Capture up to this many replies under each of those comments; zero
	/// leaves every thread collapsed.
	pub subcomments: usize,
	/// How many posts to work on at once, each in its own browser session; 1
	/// keeps everything on this thread.
	pub workers: usize,
	/// What the screenshots are encoded as.
	pub format: ImageFormat,
}

/// Captures every post on the channel, restarting the browser between batches
/// (and after a crash) so a long feed does not outlive the renderer. Writes
/// `posts.json` alongside the screenshots and returns how many were written.
pub(super) fn archive(posts_url: &str, out_dir: &Path, opts: &Options) -> Result<usize> {
	std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
	let (limit, workers) = (opts.limit, opts.workers);

	let mut next = 0;
	// Keys are zero-padded, so the map serialises in capture order.
	let mut index = serde_json::Map::new();

	loop {
		let round = if workers > 1 {
			run_round(posts_url, out_dir, next, opts)?
		} else {
			run_session(posts_url, out_dir, next, opts)?
		};

		let mut entries = resolve_details(&round.captures, opts.details, workers);
		if let Some(min_likes) = opts.comments {
			let wanted = comments::Wanted {
				min_likes,
				subcomments: opts.subcomments,
				format: opts.format,
			};
			attach_comments(&mut entries, &round.captures, out_dir, wanted, workers);
		}
		index.extend(entries);
		// Rewritten after every round, so an interrupted run still leaves a
		// usable index for the screenshots on disk.
		write_index(out_dir, posts_url, opts, &index)?;

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
fn run_session(posts_url: &str, out_dir: &Path, start: usize, opts: &Options) -> Result<Progress> {
	debug!("starting a browser session at post {}", start + 1);
	let browser = browser::launch()?;
	let tab = browser.new_tab()?;
	tab.set_default_timeout(Duration::from_secs(60));
	Archiver::new(&tab, out_dir, opts.limit, opts.format).run_batch(posts_url, start)
}

/// Runs up to `workers` browser sessions at once, on consecutive batches
/// starting at `start`, and folds them back into a single [`Progress`].
///
/// Every session loads the feed from the top, so a later batch pays for
/// scrolling past the earlier ones; the capture work, which dominates, is what
/// actually runs in parallel.
fn run_round(posts_url: &str, out_dir: &Path, start: usize, opts: &Options) -> Result<Progress> {
	let limit = opts.limit;
	let starts: Vec<usize> = (0..opts.workers).map(|w| start + w * BATCH).take_while(|s| limit.is_none_or(|l| *s < l)).collect();
	if starts.len() < 2 {
		return run_session(posts_url, out_dir, start, opts);
	}
	info!("capturing posts {}+ across {} browser sessions", start + 1, starts.len());

	let results: Vec<Result<Progress>> = std::thread::scope(|scope| {
		let handles: Vec<_> = starts.iter().map(|&s| scope.spawn(move || run_session(posts_url, out_dir, s, opts))).collect();
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

/// Collects each captured post's comments and nests them under its index entry,
/// as `comments`. A post the feed gave no id for cannot be opened, so its entry
/// keeps the `null` that says nothing could be read.
///
/// Best effort, like the details lookup: the screenshots are already on disk,
/// and a comment section that would not load should not cost us the round.
fn attach_comments(entries: &mut [(String, Value)], captures: &[Capture], out_dir: &Path, wanted: comments::Wanted, workers: usize) {
	let targets: Vec<Target> = captures
		.iter()
		.filter_map(|c| {
			Some(Target {
				index: c.index,
				id: c.id.clone()?,
			})
		})
		.collect();

	let mut collected = comments::collect(&targets, out_dir, wanted, workers).into_iter();
	for (entry, capture) in entries.iter_mut().zip(captures) {
		let found = match capture.id {
			Some(_) => collected.next().unwrap_or(Value::Null),
			None => Value::Null,
		};
		if let Some(entry) = entry.1.as_object_mut() {
			entry.insert("comments".to_string(), found);
		}
	}
}

fn write_index(out_dir: &Path, posts_url: &str, opts: &Options, index: &serde_json::Map<String, Value>) -> Result<()> {
	let path = out_dir.join(INDEX_FILE);
	let doc = serde_json::json!({
		"channel": posts_url,
		"captured_at": time::now_utc(),
		// What the nested comment lists were filtered by, so an index with few
		// of them is not mistaken for posts that were barely commented on, and
		// how many replies were kept under each, for the same reason.
		"comment_min_likes": opts.comments,
		"comment_subcomments": opts.comments.map(|_| opts.subcomments),
		"posts": index,
	});
	debug!("writing index with {} entries to {}", index.len(), path.display());
	std::fs::write(&path, serde_json::to_string_pretty(&doc)?).with_context(|| format!("writing {}", path.display()))
}

struct Archiver<'a> {
	tab: &'a Tab,
	/// Takes the screenshots themselves, off the same injected page.
	clipper: Clipper<'a>,
	out_dir: PathBuf,
	/// Stop after this many posts; used to sample a channel quickly.
	limit: Option<usize>,
	/// What the screenshots are encoded as.
	format: ImageFormat,
}

impl<'a> Archiver<'a> {
	fn new(tab: &'a Tab, out_dir: impl Into<PathBuf>, limit: Option<usize>, format: ImageFormat) -> Self {
		Self {
			tab,
			clipper: Clipper::new(tab, format),
			out_dir: out_dir.into(),
			limit,
			format,
		}
	}

	/// Loads the posts tab in dark theme and captures up to `BATCH` posts
	/// starting at `start`.
	fn run_batch(&self, posts_url: &str, start: usize) -> Result<Progress> {
		browser::emulate_dark(self.tab)?;
		self.clipper.init_viewport()?;

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
					if !self.clipper.is_alive() {
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

	fn inject(&self) -> Result<()> {
		self.eval(ARCHIVER_JS)?;
		self.eval("__yta.prepare()")?;
		Ok(())
	}

	fn eval(&self, expr: &str) -> Result<Value> {
		self.clipper.eval(expr)
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

	/// Screenshots post `i` and writes it out, returning the metadata its index
	/// entry needs; the exact-date lookup happens later, off the browser
	/// session.
	fn capture_post(&self, i: usize) -> Result<Capture> {
		let (image, info) = self.clipper.capture(i)?;

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
