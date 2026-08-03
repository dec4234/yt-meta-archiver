//! Reading the chat of a livestream that is already over.
//!
//! Once a stream ends YouTube keeps its chat as a *replay*, and serves it from a
//! different place than the live one: the popout chat page answers a finished
//! stream with "chat is disabled", so [`super::watch`] cannot see a VOD at all.
//! What can is the endpoint the replay player itself calls, which hands back the
//! chat in chunks of stream time, each naming the chunk to ask for next.
//!
//! That makes this the opposite of watching a live chat in every way that
//! matters. There is a definite end to work towards, so the run finishes on its
//! own; the messages carry their offset into the stream, so the transcript is
//! ordered by when they were said rather than by when we saw them; and nothing
//! has to be rendered, so no browser is involved and the whole chat arrives in
//! about a request per three minutes of stream — hundreds of times faster than
//! the stream took to happen.
//!
//! The cost is that there is no page, so there is nothing to screenshot: this
//! path is metadata only, and [`super::run`] says so when asked for pictures.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use log::{debug, info, trace, warn};
use serde_json::{Value, json};

use super::watch::{self, TRANSCRIPT_FILE};
use crate::{http, video};

/// The endpoint the replay chat paginates through. Its `key` is the public
/// InnerTube key every watch page carries, not a credential of ours.
const REPLAY_ENDPOINT: &str = "https://www.youtube.com/youtubei/v1/live_chat/get_live_chat_replay";

/// A guard on a continuation chain that never ends. A very long stream is a few
/// hundred rounds, so this only bites if YouTube starts handing back a token
/// that loops.
const MAX_ROUNDS: usize = 20_000;

/// How often to report progress, in rounds. Each is a couple of minutes of
/// stream, so a stream of any length still says something every few seconds.
const PROGRESS_EVERY: usize = 25;

/// The renderers worth archiving, and what we call each kind. These are the same
/// names [`super::watch`] keeps under their DOM spelling, so a replay transcript
/// and a live one describe a message the same way.
///
/// What is left out is either chrome — the engagement nudges, the "welcome to
/// live chat" notice — or the ticker bar along the top, which only restates the
/// superchats that already have a message of their own.
const KINDS: [(&str, &str); 6] = [
	("liveChatTextMessageRenderer", "text"),
	("liveChatPaidMessageRenderer", "superchat"),
	("liveChatPaidStickerRenderer", "supersticker"),
	("liveChatMembershipItemRenderer", "membership"),
	("liveChatSponsorshipsGiftPurchaseAnnouncementRenderer", "gift"),
	("liveChatSponsorshipsGiftRedemptionAnnouncementRenderer", "gift-redemption"),
];

/// What a video's watch page says about its chat, and so which of the two
/// extractors can read it.
pub(super) enum Chat {
	/// Still going: only the browser-driven live path can follow it.
	Live,
	/// Over, with its chat kept as a replay this module can read.
	Replay(Replay),
	/// The page did not say. Nothing is concluded from that — the live path is
	/// still tried, and it has its own clear answer for a stream with no chat.
	Unknown,
}

/// Everything needed to walk one stream's chat replay.
pub(super) struct Replay {
	/// The public InnerTube key, read off the watch page.
	api_key: String,
	/// The client version to claim, likewise. Sending a stale one gets the
	/// request refused, so it is taken from the page rather than pinned here.
	client_version: String,
	/// The token for the first chunk of chat; each response names the next.
	continuation: String,
}

/// Asks a video's watch page how its chat can be read.
///
/// Anything unexpected in the page is [`Chat::Unknown`] rather than an error:
/// the live path is the one that has always run, and a page we cannot make sense
/// of is no reason to stop it from trying.
pub(super) fn probe(video_id: &str) -> Result<Chat> {
	let html = http::get_html(&video::watch_url(video_id))?;
	let data = initial_data(&html).ok_or_else(|| anyhow!("no page data on the watch page for {video_id}"))?;

	let chat = &data["contents"]["twoColumnWatchNextResults"]["conversationBar"]["liveChatRenderer"];
	if !chat.is_object() {
		debug!("the watch page shows no live chat for {video_id}");
		return Ok(Chat::Unknown);
	}
	// Set only on a finished stream's chat. A live one has the same renderer
	// without it, which is what the live path expects to find.
	if !chat["isReplay"].as_bool().unwrap_or(false) {
		debug!("{video_id} has a live chat rather than a replay");
		return Ok(Chat::Live);
	}

	let continuation = chat["continuations"][0]["reloadContinuationData"]["continuation"].as_str();
	let Some(continuation) = continuation else {
		warn!("{video_id} has a chat replay but the page named no continuation for it");
		return Ok(Chat::Unknown);
	};

	Ok(Chat::Replay(Replay {
		api_key: config_string(&html, "INNERTUBE_API_KEY").ok_or_else(|| anyhow!("the watch page for {video_id} carries no API key"))?,
		client_version: config_string(&html, "INNERTUBE_CLIENT_VERSION").ok_or_else(|| anyhow!("the watch page for {video_id} carries no client version"))?,
		continuation: continuation.to_string(),
	}))
}

/// Walks the whole replay, writing the transcript as it goes. Returns how many
/// messages it added.
///
/// Like the live path this appends and deduplicates rather than truncating, so a
/// replay run over a directory that already holds a partial live capture fills
/// in what was missed instead of throwing away what was caught.
pub(super) fn archive(replay: &Replay, out_dir: &Path) -> Result<usize> {
	std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
	let path = out_dir.join(TRANSCRIPT_FILE);

	let mut recorded = existing_ids(&path)?;
	let already = recorded.len();
	if already > 0 {
		info!("{already} messages already in the transcript, adding what is missing");
	}

	let file = File::options().create(true).append(true).open(&path).with_context(|| format!("opening {}", path.display()))?;
	let mut transcript = BufWriter::new(file);

	let mut continuation = Some(replay.continuation.clone());
	let mut written = 0;
	let mut rounds = 0;
	let mut offset_ms = 0;

	while let Some(token) = continuation {
		if rounds >= MAX_ROUNDS {
			warn!("stopping after {rounds} rounds — the replay is not ending");
			break;
		}
		let response = replay.fetch(&token).with_context(|| format!("reading the chat replay {} in", stream_time(offset_ms)))?;
		let chat = &response["continuationContents"]["liveChatContinuation"];
		if !chat.is_object() {
			// The last chunk of a replay simply stops carrying one, which is how
			// a run ends; anything else here is a response we cannot read.
			debug!("no chat contents in round {}, taking the replay as finished", rounds + 1);
			break;
		}

		for (message, at_ms) in messages(chat) {
			offset_ms = offset_ms.max(at_ms);
			if let Some(id) = message["id"].as_str()
				&& !recorded.insert(id.to_string())
			{
				trace!("already have message {id}");
				continue;
			}
			let mut entry = watch::entry(&message, already + written, None);
			// What a replay has and a live capture cannot: where in the stream
			// the message was said.
			entry["offset_ms"] = at_ms.into();
			writeln!(transcript, "{}", serde_json::to_string(&entry)?).context("writing the transcript")?;
			written += 1;
			trace!("[{}] {} {}", already + written, stream_time(at_ms), watch::summarise(&entry));
		}

		rounds += 1;
		// Flushed a chunk at a time rather than a message at a time: a replay is
		// hundreds of messages a second, and unlike a live run there is no
		// operator watching the file fill up.
		transcript.flush().context("flushing the transcript")?;
		if rounds % PROGRESS_EVERY == 0 {
			info!("{} messages, {} into the stream", already + written, stream_time(offset_ms));
		}
		continuation = next_continuation(chat);
	}

	transcript.flush().context("flushing the transcript")?;
	info!("read {} of chat in {rounds} requests", stream_time(offset_ms));
	Ok(written)
}

impl Replay {
	/// One chunk of the replay.
	fn fetch(&self, continuation: &str) -> Result<Value> {
		let url = format!("{REPLAY_ENDPOINT}?key={}&prettyPrint=false", self.api_key);
		let body = json!({
			"context": {
				"client": {
					"clientName": "WEB",
					"clientVersion": self.client_version,
					// English, so the badge and membership wording a transcript
					// carries does not depend on where it was archived.
					"hl": "en",
					"gl": "US",
				},
			},
			"continuation": continuation,
		});
		http::post_json(&url, &body)
	}
}

/// The messages in one chunk, each with its offset into the stream in
/// milliseconds, in the order they were said.
fn messages(chat: &Value) -> Vec<(Value, i64)> {
	let mut out = Vec::new();
	for action in chat["actions"].as_array().map(Vec::as_slice).unwrap_or_default() {
		let replayed = &action["replayChatItemAction"];
		// The offset is on the replay wrapper, not the message: it is a property
		// of when the message was played back into the chat.
		let at_ms = replayed["videoOffsetTimeMsec"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
		for inner in replayed["actions"].as_array().map(Vec::as_slice).unwrap_or_default() {
			// Only messages being added. A replay also carries the ticker items
			// along the top of the chat, which restate superchats we already have.
			let item = &inner["addChatItemAction"]["item"];
			if let Some(message) = describe(item) {
				out.push((message, at_ms));
			}
		}
	}
	out
}

/// One chat item in the shape [`watch::entry`] wants, or `None` if it is not a
/// message a viewer sent — chrome, or one YouTube has since withheld, which
/// comes through as a placeholder with nothing in it but an id.
fn describe(item: &Value) -> Option<Value> {
	let (renderer, kind) = KINDS.iter().find_map(|(name, kind)| item[name].as_object().map(|r| (r, *kind)))?;
	let renderer = Value::Object(renderer.clone());

	// A gift purchase says nothing itself — its author and its wording both sit
	// in the header the announcement is drawn from.
	let header = &renderer["header"]["liveChatSponsorshipsHeaderRenderer"];
	let author = simple_text(&renderer["authorName"]).or_else(|| simple_text(&header["authorName"]));

	Some(json!({
		"id": renderer["id"].as_str(),
		"kind": kind,
		"author": author,
		"author_channel_id": renderer["authorExternalChannelId"].as_str(),
		"badges": badges(&renderer),
		// Absent on a sticker and on a gift purchase, which say everything they
		// have to say in `kind`, `amount` and `header`.
		"text": runs_text(&renderer["message"]),
		// Superchats and stickers only.
		"amount": simple_text(&renderer["purchaseAmountText"]),
		// The membership tier, or the gift announcement's own wording.
		"header": runs_text(&renderer["headerSubtext"]).or_else(|| runs_text(&header["primaryText"])),
		"timestamp_usec": renderer["timestampUsec"].as_str(),
	}))
}

/// "Member (6 months)", "Moderator", "Verified" — the badges beside a name.
fn badges(renderer: &Value) -> Vec<String> {
	renderer["authorBadges"]
		.as_array()
		.map(Vec::as_slice)
		.unwrap_or_default()
		.iter()
		.filter_map(|badge| {
			let badge = &badge["liveChatAuthorBadgeRenderer"];
			// The tooltip is what a viewer sees on hover, and the same string the
			// live path reads off the DOM.
			badge["tooltip"].as_str().or_else(|| badge["accessibility"]["accessibilityData"]["label"].as_str()).map(str::to_string)
		})
		.collect()
}

/// A `{"simpleText": ".."}` node, which is how YouTube spells a string that
/// carries no formatting.
fn simple_text(node: &Value) -> Option<String> {
	let text = node["simpleText"].as_str()?.trim();
	(!text.is_empty()).then(|| text.to_string())
}

/// A run list flattened to text. Emoji are their own runs rather than
/// characters, and every one carries the `:shortcode:` a viewer would have
/// typed — which is exactly what the live path recovers from the DOM, so both
/// transcripts spell a message the same way.
fn runs_text(node: &Value) -> Option<String> {
	let runs = node["runs"].as_array()?;
	let mut out = String::new();
	for run in runs {
		if let Some(text) = run["text"].as_str() {
			out.push_str(text);
		} else if let Some(emoji) = run["emoji"].as_object() {
			let emoji = Value::Object(emoji.clone());
			// Custom channel emoji have nothing but a shortcode; a plain one also
			// carries the character itself, under `emojiId`.
			let shortcode = emoji["shortcuts"][0].as_str().or_else(|| emoji["emojiId"].as_str());
			out.push_str(shortcode.unwrap_or_default());
		}
	}
	let out = out.trim();
	(!out.is_empty()).then(|| out.to_string())
}

/// The token for the chunk after this one, absent once the replay is over.
fn next_continuation(chat: &Value) -> Option<String> {
	let continuation = &chat["continuations"][0];
	// A replay normally names its next chunk under the replay key; the reload
	// key is what the very first response can come back with instead.
	for key in ["liveChatReplayContinuationData", "reloadContinuationData"] {
		if let Some(token) = continuation[key]["continuation"].as_str() {
			return Some(token.to_string());
		}
	}
	None
}

/// The ids already in a transcript, so a re-run adds what is missing rather than
/// writing the whole replay again. A file we cannot parse a line of is not a
/// reason to fail — the id set is only ever used to skip work.
fn existing_ids(path: &Path) -> Result<HashSet<String>> {
	let Ok(file) = File::open(path) else {
		return Ok(HashSet::new());
	};
	let mut ids = HashSet::new();
	for line in BufReader::new(file).lines() {
		let line = line.with_context(|| format!("reading {}", path.display()))?;
		if let Ok(entry) = serde_json::from_str::<Value>(&line)
			&& let Some(id) = entry["id"].as_str()
		{
			ids.insert(id.to_string());
		}
	}
	Ok(ids)
}

/// `var ytInitialData = {..};` from a watch page.
fn initial_data(html: &str) -> Option<Value> {
	let start = html.find("var ytInitialData = ")? + "var ytInitialData = ".len();
	let end = html[start..].find(";</script>")? + start;
	serde_json::from_str(&html[start..end]).ok()
}

/// A `"KEY":"value"` out of the config a watch page carries inline.
fn config_string(html: &str, key: &str) -> Option<String> {
	let needle = format!("\"{key}\":\"");
	let start = html.find(&needle)? + needle.len();
	let len = html[start..].find('"')?;
	Some(html[start..start + len].to_string())
}

/// Milliseconds into a stream as `1:23:45`, for the log. Chat starts before the
/// stream does, so the offset can be negative.
fn stream_time(ms: i64) -> String {
	let sign = if ms < 0 {
		"-"
	} else {
		""
	};
	let secs = ms.abs() / 1000;
	format!("{sign}{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn text_message() -> Value {
		json!({
			"liveChatTextMessageRenderer": {
				"id": "ChwKGkNNMkZsNTZObnBNREZm",
				"timestampUsec": "1773447089082651",
				"authorName": {"simpleText": "@Someone"},
				"authorExternalChannelId": "UCwnwSNvMnLnIuwi4Xmhj98g",
				"authorBadges": [{"liveChatAuthorBadgeRenderer": {"tooltip": "Member (2 years)"}}],
				"message": {"runs": [{"text": "hello "}, {"emoji": {"emojiId": "\u{1fae1}", "shortcuts": [":saluting_face:"]}}]},
			}
		})
	}

	#[test]
	fn reads_a_text_message() {
		let message = describe(&text_message()).expect("a message");
		assert_eq!(message["id"], "ChwKGkNNMkZsNTZObnBNREZm");
		assert_eq!(message["kind"], "text");
		assert_eq!(message["author"], "@Someone");
		assert_eq!(message["author_channel_id"], "UCwnwSNvMnLnIuwi4Xmhj98g");
		assert_eq!(message["badges"], json!(["Member (2 years)"]));
		assert_eq!(message["timestamp_usec"], "1773447089082651");
		// Emoji come out as the shortcode a viewer typed, as they do live.
		assert_eq!(message["text"], "hello :saluting_face:");
		assert!(message["amount"].is_null());
	}

	#[test]
	fn feeds_the_same_transcript_line_as_a_live_capture() {
		let message = describe(&text_message()).expect("a message");
		let entry = watch::entry(&message, 0, None);
		assert_eq!(entry["seq"], 1);
		assert_eq!(entry["author"], "@Someone");
		assert_eq!(entry["text"], "hello :saluting_face:");
		assert_eq!(entry["sent_utc"], "2026-03-14T00:11:29Z");
	}

	#[test]
	fn reads_a_superchat_with_its_amount() {
		let paid = json!({
			"liveChatPaidMessageRenderer": {
				"id": "ChwKGkNPM1YtWW1TbnBN",
				"timestampUsec": "1773448666529413",
				"authorName": {"simpleText": "@Payer"},
				"purchaseAmountText": {"simpleText": "£5.00"},
				"message": {"runs": [{"text": "do the wan show"}]},
			}
		});
		let message = describe(&paid).expect("a message");
		assert_eq!(message["kind"], "superchat");
		assert_eq!(message["amount"], "£5.00");
		assert_eq!(message["text"], "do the wan show");
	}

	#[test]
	fn reads_a_membership_and_its_tier() {
		let joined = json!({
			"liveChatMembershipItemRenderer": {
				"id": "ChwKGkNLTDZ3YUNXbnBN",
				"authorName": {"simpleText": "@NewMember"},
				"headerSubtext": {"runs": [{"text": "Welcome to "}, {"text": "LTT Member"}, {"text": "!"}]},
				"authorBadges": [{"liveChatAuthorBadgeRenderer": {"accessibility": {"accessibilityData": {"label": "Member (2 years)"}}}}],
			}
		});
		let message = describe(&joined).expect("a message");
		assert_eq!(message["kind"], "membership");
		assert_eq!(message["header"], "Welcome to LTT Member!");
		// A badge with no tooltip still names itself to a screen reader.
		assert_eq!(message["badges"], json!(["Member (2 years)"]));
		assert!(message["text"].is_null());
	}

	#[test]
	fn takes_a_gift_purchases_author_and_wording_from_its_header() {
		let gift = json!({
			"liveChatSponsorshipsGiftPurchaseAnnouncementRenderer": {
				"id": "ChwKGkNQN1c5TzNDejVN",
				"authorExternalChannelId": "UCDzYXgSvYqFfPLs34axTjVQ",
				"header": {"liveChatSponsorshipsHeaderRenderer": {
					"authorName": {"simpleText": "@Giver"},
					"primaryText": {"runs": [{"text": "Sent "}, {"text": "5"}, {"text": " gift memberships"}]},
				}},
			}
		});
		let message = describe(&gift).expect("a message");
		assert_eq!(message["kind"], "gift");
		assert_eq!(message["author"], "@Giver");
		assert_eq!(message["header"], "Sent 5 gift memberships");
	}

	#[test]
	fn skips_what_is_not_a_message() {
		// A withheld message, left behind as a placeholder with nothing in it.
		assert!(describe(&json!({"liveChatPlaceholderItemRenderer": {"id": "ChwKGkNKS0N5"}})).is_none());
		// Chrome, and the ticker bar's restatement of a superchat.
		assert!(describe(&json!({"liveChatViewerEngagementMessageRenderer": {"id": "x"}})).is_none());
		assert!(describe(&json!({"liveChatTickerPaidMessageItemRenderer": {"id": "x"}})).is_none());
		assert!(describe(&json!({})).is_none());
	}

	#[test]
	fn reads_a_chunk_in_the_order_it_was_said() {
		let chat = json!({"actions": [
			{"replayChatItemAction": {
				"videoOffsetTimeMsec": "178000",
				"actions": [{"addChatItemAction": {"item": text_message()}}],
			}},
			{"replayChatItemAction": {
				"videoOffsetTimeMsec": "179500",
				"actions": [
					{"addChatItemAction": {"item": json!({"liveChatTextMessageRenderer": {"id": "second", "message": {"runs": [{"text": "hi"}]}}})}},
					// Ticker items ride along in the same action list.
					{"addLiveChatTickerItemAction": {"item": json!({"liveChatTickerPaidMessageItemRenderer": {"id": "ticker"}})}},
				],
			}},
		]});

		let messages = messages(&chat);
		assert_eq!(messages.len(), 2);
		assert_eq!(messages[0].1, 178_000);
		assert_eq!(messages[1].0["id"], "second");
		assert_eq!(messages[1].1, 179_500);
	}

	#[test]
	fn follows_the_replay_continuation_and_stops_at_the_end() {
		let chat = json!({"continuations": [{"liveChatReplayContinuationData": {"continuation": "next"}}]});
		assert_eq!(next_continuation(&chat).as_deref(), Some("next"));
		// The first response can name its follow-up under the reload key instead.
		let reload = json!({"continuations": [{"reloadContinuationData": {"continuation": "first"}}]});
		assert_eq!(next_continuation(&reload).as_deref(), Some("first"));
		// The last chunk names nothing, which is what ends a run.
		assert_eq!(next_continuation(&json!({"continuations": [{}]})), None);
		assert_eq!(next_continuation(&json!({})), None);
	}

	#[test]
	fn tells_a_replay_apart_from_a_live_chat_and_from_neither() {
		let page = |chat: Value| {
			let data = json!({"contents": {"twoColumnWatchNextResults": {"conversationBar": chat}}});
			format!("<script>var ytInitialData = {data};</script>")
		};

		let replay = page(json!({"liveChatRenderer": {"isReplay": true, "continuations": [{"reloadContinuationData": {"continuation": "op2w0"}}]}}));
		let data = initial_data(&replay).expect("page data");
		assert_eq!(data["contents"]["twoColumnWatchNextResults"]["conversationBar"]["liveChatRenderer"]["isReplay"], true);

		// A live stream carries the same renderer without the replay flag.
		let live = page(json!({"liveChatRenderer": {"continuations": [{}]}}));
		assert!(initial_data(&live).expect("page data")["contents"]["twoColumnWatchNextResults"]["conversationBar"]["liveChatRenderer"]["isReplay"].is_null());

		// And a video whose chat is gone has no renderer at all.
		let none = page(json!({"conversationBarRenderer": {}}));
		assert!(initial_data(&none).expect("page data")["contents"]["twoColumnWatchNextResults"]["conversationBar"]["liveChatRenderer"].is_null());
	}

	#[test]
	fn reads_the_config_a_watch_page_carries() {
		let html = r#"ytcfg.set({"INNERTUBE_API_KEY":"AIzaSyAO_FJ2","INNERTUBE_CLIENT_VERSION":"2.20260729.00.00"});"#;
		assert_eq!(config_string(html, "INNERTUBE_API_KEY").as_deref(), Some("AIzaSyAO_FJ2"));
		assert_eq!(config_string(html, "INNERTUBE_CLIENT_VERSION").as_deref(), Some("2.20260729.00.00"));
		assert_eq!(config_string(html, "NOT_THERE"), None);
	}

	#[test]
	fn formats_stream_time_either_side_of_the_start() {
		assert_eq!(stream_time(0), "0:00:00");
		assert_eq!(stream_time(2_554_000), "0:42:34");
		assert_eq!(stream_time(4_757_000), "1:19:17");
		// Chat opens before the stream does.
		assert_eq!(stream_time(-888_000), "-0:14:48");
	}
}
