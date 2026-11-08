//! A finished stream's chat, read end to end from the replay API: the watch
//! page is probed, the continuation chain is walked, and the transcript is
//! written, with profile pictures and custom emoji saved beside it. A local
//! server plays YouTube.

mod common;

use common::{MockServer, Request, Response, TempDir, read_jsonl};
use serde_json::{Value, json};
use yt_meta_archiver::image_store::ImageStore;
use yt_meta_archiver::livechat::TRANSCRIPT_FILE;
use yt_meta_archiver::livechat::replay::{self, Chat, Replay};
use yt_meta_archiver::profile_picture::ProfilePictures;
use yt_meta_archiver::{http, profile_picture};

const REPLAY_PATH: &str = "/youtubei/v1/live_chat/get_live_chat_replay";
const API_KEY: &str = "AIzaTestKey";
const CLIENT_VERSION: &str = "2.20260729.00.00";

/// The watch page of a stream that is over, naming `c0` as its first chunk.
fn watch_page() -> String {
	let data = json!({"contents": {"twoColumnWatchNextResults": {"conversationBar": {"liveChatRenderer": {
		"isReplay": true,
		"continuations": [{"reloadContinuationData": {"continuation": "c0"}}],
	}}}}});
	format!(r#"<html><script>ytcfg.set({{"INNERTUBE_API_KEY":"{API_KEY}","INNERTUBE_CLIENT_VERSION":"{CLIENT_VERSION}"}});</script><script>var ytInitialData = {data};</script></html>"#)
}

/// The server this request reached, so the canned chat can point at images on
/// it.
fn base(request: &Request) -> String {
	format!("http://{}", request.header("host").expect("a host header"))
}

fn text_message(id: &str, author: &str, text: &str, photo: &str) -> Value {
	json!({"liveChatTextMessageRenderer": {
		"id": id,
		"timestampUsec": "1700000000000000",
		"authorName": {"simpleText": author},
		"authorExternalChannelId": "UCaaaaaaaaaaaaaaaaaaaaaa",
		"authorPhoto": {"thumbnails": [{"url": format!("{photo}=s32")}, {"url": format!("{photo}=s64")}]},
		"message": {"runs": [{"text": text}]},
	}})
}

/// One replay action: `items` played back `offset_ms` into the stream.
fn at(offset_ms: u64, items: Vec<Value>) -> Value {
	json!({"replayChatItemAction": {
		"videoOffsetTimeMsec": offset_ms.to_string(),
		"actions": items.into_iter().map(|item| json!({"addChatItemAction": {"item": item}})).collect::<Vec<_>>(),
	}})
}

fn chunk(actions: Vec<Value>, next: Option<&str>) -> Value {
	let mut chat = json!({"actions": actions});
	if let Some(next) = next {
		chat["continuations"] = json!([{"liveChatReplayContinuationData": {"continuation": next}}]);
	}
	json!({"continuationContents": {"liveChatContinuation": chat}})
}

/// YouTube, for one stream: its watch page, two chunks of chat, and the images
/// they point at. A third chunk, `c2`, fails, for the tests that want it to.
fn youtube() -> MockServer {
	MockServer::start(|request| match request.path() {
		"/watch" => Response::html(watch_page()),
		REPLAY_PATH => {
			let base = base(request);
			let photo = format!("{base}/pp/UCaaa");
			match request.json()["continuation"].as_str() {
				Some("c0") => Response::json(&chunk(
					vec![
						at(1_000, vec![text_message("m1", "@First", "hello", &photo)]),
						at(
							2_500,
							vec![json!({"liveChatTextMessageRenderer": {
								"id": "m2",
								"authorName": {"simpleText": "@Second"},
								"message": {"runs": [
									{"text": "look "},
									{"emoji": {"emojiId": "UCchan/emote", "isCustomEmoji": true, "shortcuts": [":_emote:"], "image": {"thumbnails": [{"url": format!("{base}/emoji/e=w48-h48")}]}}},
								]},
							}})],
						),
					],
					Some("c1"),
				)),
				Some("c1") => Response::json(&chunk(
					vec![
						// The same message again, as a chunk boundary can repeat one.
						at(2_500, vec![text_message("m1", "@First", "hello", &photo)]),
						at(
							61_000,
							vec![
								json!({"liveChatPaidMessageRenderer": {
									"id": "m3",
									"authorName": {"simpleText": "@Payer"},
									"purchaseAmountText": {"simpleText": "$5.00"},
									"message": {"runs": [{"text": "great stream"}]},
								}}),
								// Chrome that is not a message.
								json!({"liveChatViewerEngagementMessageRenderer": {"id": "chrome"}}),
							],
						),
					],
					None,
				)),
				Some("c1-then-fail") => Response::json(&chunk(vec![at(500, vec![text_message("m9", "@Early", "first", &photo)])], Some("c2"))),
				_ => Response::status(500),
			}
		}
		path if path.starts_with("/pp/") => Response::ok("image/jpeg", b"jpeg".to_vec()),
		path if path.starts_with("/emoji/") => Response::ok("image/png", b"png".to_vec()),
		_ => Response::status(404),
	})
}

/// The replay the watch page on `server` names, pointed back at `server`.
fn probe(server: &MockServer) -> Replay {
	let html = http::get_html(&server.url("/watch?v=dQw4w9WgXcQ")).unwrap();
	match replay::read_watch_page(&html, "dQw4w9WgXcQ").unwrap() {
		Chat::Replay(replay) => replay.with_endpoint(server.url(REPLAY_PATH)),
		other => panic!("want a replay, got {other:?}"),
	}
}

#[test]
fn reads_the_replay_config_off_the_watch_page() {
	let server = youtube();
	let replay = probe(&server);
	assert_eq!(replay.api_key(), API_KEY);
	assert_eq!(replay.client_version(), CLIENT_VERSION);
	assert_eq!(replay.continuation(), "c0");
}

#[test]
fn walks_the_whole_replay_into_a_transcript() {
	let server = youtube();
	let out = TempDir::new("replay-walk");

	let written = replay::archive(&probe(&server), out.path(), None, None).unwrap();
	assert_eq!(written, 3);

	let lines = read_jsonl(&out.path().join(TRANSCRIPT_FILE));
	let ids: Vec<_> = lines.iter().map(|l| l["id"].as_str().unwrap()).collect();
	assert_eq!(ids, ["m1", "m2", "m3"], "in the order said, the repeat and the chrome left out");
	let seqs: Vec<_> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
	assert_eq!(seqs, [1, 2, 3]);
	let offsets: Vec<_> = lines.iter().map(|l| l["offset_ms"].as_i64().unwrap()).collect();
	assert_eq!(offsets, [1_000, 2_500, 61_000]);

	assert_eq!(lines[0]["author"], "@First");
	assert_eq!(lines[0]["text"], "hello");
	assert_eq!(lines[0]["sent_utc"], "2023-11-14T22:13:20Z");
	assert_eq!(lines[1]["text"], "look :_emote:");
	assert_eq!(lines[1]["emojis"][0]["shortcut"], ":_emote:");
	assert_eq!(lines[2]["kind"], "superchat");
	assert_eq!(lines[2]["amount"], "$5.00");
	// Nothing was asked for beyond the transcript.
	assert!(lines.iter().all(|l| l.get("profile_picture").is_none()));
	assert!(lines[1]["emojis"][0].get("image").is_none());
	assert!(server.requests_to("/pp/UCaaa=s176").is_empty());
}

#[test]
fn asks_for_each_chunk_the_way_the_player_does() {
	let server = youtube();
	let out = TempDir::new("replay-requests");
	replay::archive(&probe(&server), out.path(), None, None).unwrap();

	let requests = server.requests_to(REPLAY_PATH);
	let continuations: Vec<_> = requests.iter().map(|r| r.json()["continuation"].as_str().unwrap().to_string()).collect();
	assert_eq!(continuations, ["c0", "c1"], "follows the chain and stops where it ends");
	for request in &requests {
		assert_eq!(request.method, "POST");
		assert_eq!(request.query("key"), Some(API_KEY));
		assert_eq!(request.query("prettyPrint"), Some("false"));
		let client = &request.json()["context"]["client"];
		assert_eq!(client["clientName"], "WEB");
		assert_eq!(client["clientVersion"], CLIENT_VERSION);
		assert_eq!(client["hl"], "en");
	}
}

#[test]
fn saves_profile_pictures_and_custom_emoji_when_asked() {
	let server = youtube();
	let out = TempDir::new("replay-images");
	let images = TempDir::new("replay-images-store");
	let pictures = ProfilePictures::new(ImageStore::create(&images.path().join("pp"), "profile picture", false).unwrap(), profile_picture::SIZE);
	let emojis = ImageStore::create(&images.path().join("emoji"), "emoji", false).unwrap();

	replay::archive(&probe(&server), out.path(), Some(&pictures), Some(&emojis)).unwrap();
	let lines = read_jsonl(&out.path().join(TRANSCRIPT_FILE));

	// Filed under the author's channel id, at the one size every run uses.
	assert!(lines[0]["profile_picture"].as_str().is_some_and(|p| p.ends_with("/UCaaaaaaaaaaaaaaaaaaaaaa.jpg")), "{}", lines[0]);
	assert_eq!(server.requests_to("/pp/UCaaa=s176").len(), 1);
	assert!(images.path().join("pp/UCaaaaaaaaaaaaaaaaaaaaaa.jpg").is_file());
	// No channel id on these, so nothing to file a picture under.
	assert!(lines[1]["profile_picture"].is_null());
	assert!(lines[2]["profile_picture"].is_null());

	assert!(lines[1]["emojis"][0]["image"].as_str().is_some_and(|p| p.ends_with("/UCchan_emote.png")), "{}", lines[1]);
	assert_eq!(std::fs::read(images.path().join("emoji/UCchan_emote.png")).unwrap(), b"png");
	assert_eq!(pictures.store().counts(), (1, 0));
	assert_eq!(emojis.counts(), (1, 0));
}

#[test]
fn a_second_run_adds_nothing_it_already_has() {
	let server = youtube();
	let out = TempDir::new("replay-rerun");
	let replay = probe(&server);

	assert_eq!(replay::archive(&replay, out.path(), None, None).unwrap(), 3);
	assert_eq!(replay::archive(&replay, out.path(), None, None).unwrap(), 0);
	assert_eq!(read_jsonl(&out.path().join(TRANSCRIPT_FILE)).len(), 3);
}

#[test]
fn fills_in_what_a_partial_live_capture_missed() {
	let server = youtube();
	let out = TempDir::new("replay-fill");
	std::fs::write(out.path().join(TRANSCRIPT_FILE), "{\"seq\":1,\"id\":\"m2\",\"text\":\"caught live\"}\n").unwrap();

	assert_eq!(replay::archive(&probe(&server), out.path(), None, None).unwrap(), 2);

	let lines = read_jsonl(&out.path().join(TRANSCRIPT_FILE));
	let ids: Vec<_> = lines.iter().map(|l| l["id"].as_str().unwrap()).collect();
	assert_eq!(ids, ["m2", "m1", "m3"], "the live line is kept as it was");
	assert_eq!(lines[0]["text"], "caught live");
	// Numbered on from what was already there.
	assert_eq!(lines[1]["seq"], 2);
	assert_eq!(lines[2]["seq"], 3);
}

#[test]
fn a_failed_chunk_stops_the_run_but_keeps_what_came_before() {
	let server = youtube();
	let out = TempDir::new("replay-fail");
	let replay = Replay::new(API_KEY, CLIENT_VERSION, "c1-then-fail").with_endpoint(server.url(REPLAY_PATH));

	let err = replay::archive(&replay, out.path(), None, None).unwrap_err();
	assert!(format!("{err:#}").contains("reading the chat replay"), "{err:#}");

	// The first chunk was flushed before the second was asked for.
	let lines = read_jsonl(&out.path().join(TRANSCRIPT_FILE));
	assert_eq!(lines.len(), 1);
	assert_eq!(lines[0]["id"], "m9");
}

#[test]
fn an_answer_without_chat_ends_the_run() {
	let server = MockServer::start(|_| Response::json(&json!({"responseContext": {}})));
	let out = TempDir::new("replay-empty");
	let replay = Replay::new(API_KEY, CLIENT_VERSION, "c0").with_endpoint(server.url(REPLAY_PATH));

	assert_eq!(replay::archive(&replay, out.path(), None, None).unwrap(), 0);
	assert_eq!(server.requests().len(), 1);
	// The transcript exists, empty, so a later run has a file to add to.
	assert_eq!(std::fs::read_to_string(out.path().join(TRANSCRIPT_FILE)).unwrap(), "");
}

#[test]
fn a_live_stream_is_left_to_the_browser() {
	let server = MockServer::start(|_| {
		let data = json!({"contents": {"twoColumnWatchNextResults": {"conversationBar": {"liveChatRenderer": {"continuations": [{}]}}}}});
		Response::html(format!("<script>var ytInitialData = {data};</script>"))
	});
	let html = http::get_html(&server.url("/watch?v=dQw4w9WgXcQ")).unwrap();
	assert!(matches!(replay::read_watch_page(&html, "dQw4w9WgXcQ").unwrap(), Chat::Live));
}
