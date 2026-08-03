//! Driving the popout live chat: poll it for new messages and write each one
//! out as it arrives.
//!
//! Unlike the posts extractor there is no total to work through — the stream
//! decides when this ends. So the transcript is appended to and flushed a
//! message at a time, and a run interrupted at any point leaves a complete file
//! for everything said up to then.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use headless_chrome::Tab;
use headless_chrome::protocol::cdp::Page;
use log::{Level, debug, info, log_enabled, trace, warn};
use serde_json::Value;

use super::source::Stream;
use crate::browser::ImageFormat;
use crate::{browser, time};

const LIVECHAT_JS: &str = include_str!("js/livechat.js");

/// Transcript written into the run's directory, one JSON object per line so it
/// can be appended to and read back while the stream is still going.
pub(super) const TRANSCRIPT_FILE: &str = "chat.jsonl";

/// The chat is a narrow column, so the viewport is sized to it: wide enough for
/// the widest message, tall enough that a message stays on screen for a good
/// few polls before chat scrolls it away.
const CHAT_VIEWPORT: (u32, u32) = (480, 1400);

/// How often we ask the page what is new. Fast enough that a message is still
/// on screen when we go to screenshot it, slow enough not to spin.
const POLL_INTERVAL: Duration = Duration::from_millis(700);

/// Messages taken from a single poll. A busy chat simply carries the remainder
/// over to the next one.
const MAX_PER_POLL: usize = 40;

/// How long the chat may go completely silent before we say so. Only a log
/// line: a quiet stream is not a broken one.
const QUIET_NOTICE: Duration = Duration::from_secs(300);

/// Sessions in a row that archive nothing at all before we accept this is not
/// going to work. Reconnecting is otherwise unbounded: a stream routinely
/// outlasts a browser, and giving up on a chat that is still going would lose
/// the rest of it.
const MAX_BARREN_SESSIONS: u32 = 10;

/// A pause before reconnecting, so a chat that is failing for its own reasons
/// is not hammered.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// How many goes a message gets at being captured while it is still moving.
/// Messages arrive a second or so apart at worst and a round trip is far
/// quicker, so this is generous; past it the picture is dropped rather than
/// kept showing the wrong message.
const CAPTURE_ATTEMPTS: u32 = 3;

/// Watches `stream`'s chat until the process is interrupted or the chat closes,
/// appending to the transcript as it goes. Returns how many messages it wrote.
pub(super) fn watch(stream: &Stream, out_dir: &Path, pics: bool, format: ImageFormat) -> Result<usize> {
	std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

	let path = out_dir.join(TRANSCRIPT_FILE);
	// Appended to, not truncated: re-running against the same stream after a
	// drop continues the transcript rather than throwing it away.
	let file = File::options().create(true).append(true).open(&path).with_context(|| format!("opening {}", path.display()))?;

	let mut watcher = Watcher {
		out_dir: out_dir.to_path_buf(),
		transcript: BufWriter::new(file),
		pics,
		format,
		written: 0,
		recorded: HashSet::new(),
	};
	watcher.run(stream)?;
	Ok(watcher.written)
}

/// Why a browser session stopped.
enum Ended {
	/// The chat itself closed, so there is nothing left to watch.
	ChatClosed,
	/// The browser went away under us; the chat may well still be running.
	BrowserLost,
}

struct Watcher {
	out_dir: PathBuf,
	transcript: BufWriter<File>,
	/// Screenshot each message as well as recording it.
	pics: bool,
	/// What those screenshots are encoded as.
	format: ImageFormat,
	written: usize,
	/// Ids already in the transcript, so a reconnect that re-reads part of the
	/// page cannot write a message twice.
	recorded: HashSet<String>,
}

impl Watcher {
	/// Watches until the chat closes, reconnecting when the browser dies. A
	/// stream runs for hours and the browser does not always last that long, so
	/// losing it is treated as something to recover from rather than the end of
	/// the run.
	fn run(&mut self, stream: &Stream) -> Result<()> {
		// The first session skips whatever backlog YouTube rendered; a
		// reconnect deliberately does not, so messages sent while we were away
		// are recovered. Duplicates are impossible either way — `recorded` is
		// what decides whether a message is new, not the page.
		let mut prime = true;
		// Counts only sessions that achieved nothing. Losing the browser is
		// routine over a long stream, so what has to be bounded is failing
		// without progress, not reconnecting as such.
		let mut barren = 0;

		loop {
			let before = self.written;
			let outcome = self.session(stream, prime);
			if self.written > before {
				barren = 0;
			} else {
				barren += 1;
			}

			match outcome {
				Ok(Ended::ChatClosed) => return Ok(()),
				Ok(Ended::BrowserLost) => {}
				Err(e) if barren > MAX_BARREN_SESSIONS => {
					return Err(e.context(format!("live chat could not be watched, {barren} attempts running")));
				}
				Err(e) => warn!("chat session failed: {e:#}"),
			}
			if barren > MAX_BARREN_SESSIONS {
				bail!("gave up after {barren} attempts that archived nothing");
			}

			info!("reconnecting to the chat");
			// Only the very first session skips the backlog; a reconnect reads
			// it, which is how messages sent while we were away are recovered.
			prime = false;
			sleep(RECONNECT_DELAY);
		}
	}

	/// One browser session, watching until the chat closes or the browser dies.
	fn session(&mut self, stream: &Stream, prime: bool) -> Result<Ended> {
		let browser = browser::launch()?;
		let tab = browser.new_tab()?;
		tab.set_default_timeout(Duration::from_secs(60));
		self.open_chat(&tab, stream, prime)?;

		let mut last_message = Instant::now();
		let mut quiet_reported = false;

		loop {
			let batch = match self.poll(&tab) {
				Ok(batch) => batch,
				Err(e) => {
					// A dead connection means this browser is gone; hand back so
					// the caller can start a fresh one and pick the chat up.
					if !is_alive(&tab) {
						warn!("browser session ended: {e:#}");
						return Ok(Ended::BrowserLost);
					}
					warn!("poll failed: {e:#}");
					sleep(POLL_INTERVAL);
					continue;
				}
			};

			if !batch.is_empty() {
				last_message = Instant::now();
				quiet_reported = false;
			}
			for message in &batch {
				self.record(&tab, message)?;
			}
			// Capturing scrolls the list about, which stops YouTube's own
			// auto-scrolling and makes it queue new messages; this hands the
			// chat back so the next poll sees them.
			if self.pics && !batch.is_empty() {
				let _ = browser::eval(&tab, "__ytc.resume()");
			}

			// Only worth asking once we have nothing else to do; a chat that is
			// still delivering messages is plainly still live.
			if batch.is_empty() {
				match chat_is_live(&tab) {
					Ok(true) => {}
					Ok(false) => {
						info!("live chat closed");
						return Ok(Ended::ChatClosed);
					}
					Err(e) if !is_alive(&tab) => {
						warn!("browser session ended: {e:#}");
						return Ok(Ended::BrowserLost);
					}
					Err(e) => warn!("could not check whether chat is still live: {e:#}"),
				}
			}
			// Costs a round trip, so it is only asked for when it will be read.
			// Worth having: it distinguishes a quiet chat from one whose
			// responses we have stopped seeing.
			if !batch.is_empty()
				&& log_enabled!(Level::Debug)
				&& let Ok(state) = browser::eval(&tab, "__ytc.state()")
			{
				debug!("chat state: {state}");
			}
			if !quiet_reported && last_message.elapsed() > QUIET_NOTICE {
				info!("no messages for {} minutes, still watching", QUIET_NOTICE.as_secs() / 60);
				quiet_reported = true;
			}

			sleep(POLL_INTERVAL);
		}
	}

	/// Loads the popout chat in dark theme, optionally skipping whatever backlog
	/// YouTube rendered so the transcript starts where we started watching.
	fn open_chat(&mut self, tab: &Tab, stream: &Stream, prime: bool) -> Result<()> {
		browser::emulate_dark(tab)?;
		browser::set_viewport(tab, CHAT_VIEWPORT.0, CHAT_VIEWPORT.1)?;

		// Registered before we navigate, and so re-run at the start of every
		// document we load from here. This is what gets the response hook in
		// ahead of YouTube's own bundle: injected any later, the app already
		// holds its own reference to `fetch` and our wrapper never sees the
		// chat continuations at all.
		tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
			source: LIVECHAT_JS.to_string(),
			world_name: None,
			include_command_line_api: None,
			run_immediately: Some(true),
		})
		.context("installing the chat response hook")?;

		let url = super::source::chat_url(&stream.video_id);
		debug!("loading {url}");
		tab.navigate_to(&url)?.wait_until_navigated()?;
		browser::set_dark_theme_cookie(tab)?;
		tab.reload(false, None)?.wait_until_navigated()?;

		// The chat renders after its own fetch, so the item list is not there
		// the instant navigation settles.
		let deadline = Instant::now() + Duration::from_secs(30);
		loop {
			let state = browser::eval(tab, LIVECHAT_JS).and_then(|_| browser::eval(tab, "__ytc.prepare()"))?;
			if state["ready"].as_bool().unwrap_or(false) {
				break;
			}
			if Instant::now() > deadline {
				bail!("live chat never loaded at {url} — the stream may have ended or have chat turned off");
			}
			sleep(Duration::from_millis(500));
		}

		if prime {
			// Held in `recorded`, the same set that keeps a reconnect from
			// writing a message twice, so the backlog stays out for good rather
			// than only until the page next reloads itself.
			let primed = browser::eval(tab, "__ytc.prime()")?;
			let ids = primed.as_array().map(Vec::as_slice).unwrap_or_default();
			for id in ids.iter().filter_map(Value::as_str) {
				self.recorded.insert(id.to_string());
			}
			debug!("skipped {} messages already on the page", ids.len());
		}
		Ok(())
	}

	/// Everything said since the last poll.
	fn poll(&self, tab: &Tab) -> Result<Vec<Value>> {
		let batch = browser::eval(tab, &format!("__ytc.poll({MAX_PER_POLL})"))?;
		Ok(batch.as_array().cloned().unwrap_or_default())
	}

	/// Screenshots a message if asked to, then appends its line to the
	/// transcript and flushes, so the file on disk is never behind what we have
	/// logged.
	fn record(&mut self, tab: &Tab, message: &Value) -> Result<()> {
		// A reconnect re-reads messages we may already hold, so identity, not
		// arrival, is what decides whether this is new.
		if let Some(id) = message["id"].as_str()
			&& !self.recorded.insert(id.to_string())
		{
			trace!("already have message {id}");
			return Ok(());
		}

		let pic = if self.pics {
			self.capture(tab, message)
		} else {
			None
		};

		let entry = entry(message, self.written, pic.as_deref());
		writeln!(self.transcript, "{}", serde_json::to_string(&entry)?).context("writing the transcript")?;
		self.transcript.flush().context("flushing the transcript")?;
		self.written += 1;

		info!("[{}] {}", self.written, summarise(&entry));
		Ok(())
	}

	/// A screenshot of one message, or `None` if it could not be taken. Chat
	/// scrolls on its own, so a message can leave the viewport between being
	/// polled and being captured; that costs a picture, never the message.
	fn capture(&self, tab: &Tab, message: &Value) -> Option<String> {
		let id = message["id"].as_str()?;
		match self.try_capture(tab, id) {
			Ok(file) => file,
			Err(e) => {
				debug!("no screenshot for {id}: {e:#}");
				None
			}
		}
	}

	/// Measuring a message and photographing it are two round trips, and chat
	/// moves between them: one message arriving while the list is pinned to the
	/// bottom shifts everything up a row, and the clip we asked for then holds
	/// the tail of our message and the whole of whoever spoke next. So the
	/// message is measured again afterwards, and a picture is only kept if it
	/// had not moved — otherwise the fresh measurement is what the next go uses.
	fn try_capture(&self, tab: &Tab, id: &str) -> Result<Option<String>> {
		let mut info = locate(tab, id)?;
		for _ in 0..CAPTURE_ATTEMPTS {
			let Some(clip) = clip(&info, id)? else {
				return Ok(None);
			};
			let image = browser::capture(tab, clip, self.format)?;

			let after = locate(tab, id)?;
			if !moved(&info, &after) {
				let file = file_name(self.written, id, self.format);
				let path = self.out_dir.join(&file);
				std::fs::write(&path, image).with_context(|| format!("writing {}", path.display()))?;
				return Ok(Some(file));
			}
			trace!("message {id} moved while it was being captured");
			info = after;
		}
		trace!("message {id} would not hold still long enough to be captured");
		Ok(None)
	}
}

/// Where message `id` is on the page right now.
fn locate(tab: &Tab, id: &str) -> Result<Value> {
	browser::eval(tab, &format!("__ytc.locate({})", serde_json::to_string(id)?))
}

/// The capture clip for a located message, or `None` if there is nothing there
/// to photograph.
fn clip(info: &Value, id: &str) -> Result<Option<Page::Viewport>> {
	if !info["exists"].as_bool().unwrap_or(false) {
		trace!("message {id} left the page before it could be captured");
		return Ok(None);
	}
	if !info["onScreen"].as_bool().unwrap_or(false) {
		trace!("message {id} scrolled out of view before it could be captured");
		return Ok(None);
	}

	let rect = &info["rect"];
	let clip = Page::Viewport {
		x: rect["x"].as_f64().unwrap_or(0.0),
		y: rect["y"].as_f64().unwrap_or(0.0),
		width: rect["width"].as_f64().unwrap_or(0.0),
		height: rect["height"].as_f64().unwrap_or(0.0),
		scale: 2.0, // capture at 2x for legible text
	};
	if clip.width <= 0.0 || clip.height <= 0.0 {
		bail!("message has no size");
	}
	Ok(Some(clip))
}

/// Whether a message's box has shifted between two measurements. A message that
/// has gone from the page in the meantime counts as having moved, which puts the
/// picture we took of it on the same footing: not to be kept.
fn moved(before: &Value, after: &Value) -> bool {
	let side = |v: &Value, k: &str| v["rect"][k].as_f64();
	// Rounding wobble in a fractional edge is not a scroll; anything past half a
	// pixel is. A side we cannot read at all is not evidence it stayed put.
	["x", "y", "width", "height"].iter().any(|k| match (side(before, k), side(after, k)) {
		(Some(a), Some(b)) => (a - b).abs() > 0.5,
		_ => true,
	})
}

/// Whether the page still has a live chat on it. The injected helpers are
/// re-applied first: YouTube re-renders the chat on its own, and a lost `__ytc`
/// should read as "check again", not "the stream ended".
fn chat_is_live(tab: &Tab) -> Result<bool> {
	browser::eval(tab, LIVECHAT_JS)?;
	Ok(browser::eval(tab, "__ytc.state()")?["live"].as_bool().unwrap_or(false))
}

/// Whether the page is still reachable, i.e. the browser has not gone away.
fn is_alive(tab: &Tab) -> bool {
	browser::eval(tab, "1").is_ok()
}

/// One transcript line: what was said, by whom, and when we saw it.
///
/// `at` is our own arrival time, always present. `sent_utc` is YouTube's own
/// timestamp for the message, which is the one to trust when it is there.
///
/// Shared with [`super::replay`], which reads the same fields off the API rather
/// than the DOM: a transcript should describe a message the same way whether it
/// was caught live or read back afterwards.
pub(super) fn entry(message: &Value, seq: usize, pic: Option<&str>) -> Value {
	let mut entry = serde_json::Map::new();
	entry.insert("seq".to_string(), (seq + 1).into());
	entry.insert("id".to_string(), message["id"].clone());
	entry.insert("at".to_string(), time::now_utc().into());
	entry.insert("sent_utc".to_string(), sent_utc(message).into());
	entry.insert("kind".to_string(), message["kind"].clone());
	entry.insert("author".to_string(), message["author"].clone());
	entry.insert("author_channel_id".to_string(), message["author_channel_id"].clone());
	entry.insert("badges".to_string(), message["badges"].clone());
	entry.insert("text".to_string(), message["text"].clone());

	// Only on the messages that have them, rather than a wall of nulls on every
	// ordinary line.
	for field in ["amount", "header"] {
		if !message[field].is_null() {
			entry.insert(field.to_string(), message[field].clone());
		}
	}
	if let Some(pic) = pic {
		entry.insert("pic".to_string(), pic.into());
	}
	Value::Object(entry)
}

/// YouTube's own send time, converted from microseconds since the epoch.
fn sent_utc(message: &Value) -> Option<String> {
	let usec: i64 = message["timestamp_usec"].as_str()?.parse().ok()?;
	Some(time::from_epoch_secs(usec / 1_000_000))
}

/// A one-line rendering of a message, for the console.
pub(super) fn summarise(entry: &Value) -> String {
	let author = entry["author"].as_str().unwrap_or("(unknown)");
	let text = entry["text"].as_str().unwrap_or("");
	let kind = entry["kind"].as_str().unwrap_or("text");
	if kind == "text" {
		return format!("{author}: {text}");
	}
	// Superchats and memberships lead with what they are, and what they cost
	// when the message says.
	let tag = match entry["amount"].as_str() {
		Some(amount) => format!("{kind} {amount}"),
		None => kind.to_string(),
	};
	format!("{author} [{tag}]: {text}")
}

/// `00001_ChwKGkNQ.png`, keeping the transcript's ordering sortable. Chat ids
/// are long, so only the leading characters go in the name.
fn file_name(seq: usize, id: &str, format: ImageFormat) -> String {
	let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(24).collect();
	let id = if id.is_empty() {
		"unknown".to_string()
	} else {
		id
	};
	format!("{:05}_{}.{}", seq + 1, id, format.extension())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn message() -> Value {
		serde_json::json!({
			"id": "ChwKGkNQdi1oZXk",
			"kind": "text",
			"author": "Someone",
			"author_channel_id": "UC123",
			"badges": ["Member (6 months)"],
			"text": "hello :wave:",
			"amount": null,
			"header": null,
			"timestamp_usec": "1700000000123456",
		})
	}

	#[test]
	fn builds_a_transcript_line() {
		let entry = entry(&message(), 0, Some("00001_ChwKGkNQdi1oZXk.png"));
		assert_eq!(entry["seq"], 1);
		assert_eq!(entry["author"], "Someone");
		assert_eq!(entry["text"], "hello :wave:");
		assert_eq!(entry["sent_utc"], "2023-11-14T22:13:20Z");
		assert_eq!(entry["pic"], "00001_ChwKGkNQdi1oZXk.png");
		// Fields that do not apply to an ordinary message stay off the line.
		assert!(entry.get("amount").is_none());
		assert!(entry.get("header").is_none());
	}

	#[test]
	fn keeps_superchat_extras_and_omits_a_missing_pic() {
		let mut paid = message();
		paid["kind"] = "superchat".into();
		paid["amount"] = "$5.00".into();
		let entry = entry(&paid, 4, None);
		assert_eq!(entry["seq"], 5);
		assert_eq!(entry["amount"], "$5.00");
		assert!(entry.get("pic").is_none());
	}

	#[test]
	fn survives_a_message_missing_everything_optional() {
		let entry = entry(&serde_json::json!({"id": "x", "kind": "text"}), 0, None);
		assert!(entry["author"].is_null());
		assert!(entry["sent_utc"].is_null());
		assert!(entry["text"].is_null());
	}

	#[test]
	fn names_files_in_transcript_order() {
		assert_eq!(file_name(0, "ChwKGkNQdi1oZXk", ImageFormat::Png), "00001_ChwKGkNQdi1oZXk.png");
		assert_eq!(file_name(41, "a/b*c", ImageFormat::Png), "00042_abc.png");
		assert_eq!(file_name(0, "///", ImageFormat::Png), "00001_unknown.png");
	}

	fn located(y: f64, height: f64) -> Value {
		serde_json::json!({
			"exists": true,
			"onScreen": true,
			"rect": {"x": 8.0, "y": y, "width": 464.0, "height": height},
		})
	}

	#[test]
	fn keeps_a_picture_of_a_message_that_stayed_put() {
		assert!(!moved(&located(600.0, 52.0), &located(600.0, 52.0)));
		// Half a pixel is the rounding of a fractional edge, not a scroll.
		assert!(!moved(&located(600.0, 52.0), &located(600.4, 52.0)));
	}

	#[test]
	fn spots_a_message_the_chat_scrolled_out_from_under() {
		// A message arriving below shifts this one up by its own height, which
		// is what puts the next speaker in the picture.
		assert!(moved(&located(600.0, 52.0), &located(574.0, 52.0)));
		// Emoji finishing loading can reflow the message itself.
		assert!(moved(&located(600.0, 52.0), &located(600.0, 78.0)));
	}

	#[test]
	fn treats_a_message_that_left_the_page_as_moved() {
		assert!(moved(&located(600.0, 52.0), &serde_json::json!({"exists": false})));
	}

	#[test]
	fn does_not_clip_a_message_that_cannot_be_photographed() {
		assert!(clip(&serde_json::json!({"exists": false}), "x").unwrap().is_none());
		let offscreen = serde_json::json!({"exists": true, "onScreen": false, "rect": {"x": 8, "y": 0, "width": 464, "height": 52}});
		assert!(clip(&offscreen, "x").unwrap().is_none());
	}

	#[test]
	fn clips_a_located_message_at_2x() {
		let clip = clip(&located(600.0, 52.0), "x").unwrap().expect("clip");
		assert_eq!((clip.x, clip.y, clip.width, clip.height), (8.0, 600.0, 464.0, 52.0));
		assert_eq!(clip.scale, 2.0);
	}

	#[test]
	fn names_compressed_files_by_their_format() {
		assert_eq!(file_name(0, "ChwKGkNQdi1oZXk", ImageFormat::Webp(80)), "00001_ChwKGkNQdi1oZXk.webp");
	}
}
