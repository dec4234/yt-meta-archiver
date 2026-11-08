//! What a user may paste, through the public functions that read it: each
//! input names one video or one channel, and the URLs built back from it lead
//! to the same place.

use yt_meta_archiver::channel::{self, Channel};
use yt_meta_archiver::livechat::source;
use yt_meta_archiver::posts::details;
use yt_meta_archiver::{time, video};

const VIDEO: &str = "dQw4w9WgXcQ";

#[test]
fn every_way_of_naming_a_video_resolves_to_its_id() {
	for input in [
		VIDEO,
		"https://www.youtube.com/watch?v=dQw4w9WgXcQ",
		"https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=1",
		"https://m.youtube.com/watch?v=dQw4w9WgXcQ",
		"https://youtu.be/dQw4w9WgXcQ",
		"https://www.youtube.com/live/dQw4w9WgXcQ?si=abc",
		"https://www.youtube.com/shorts/dQw4w9WgXcQ",
		"https://www.youtube.com/embed/dQw4w9WgXcQ",
		"https://www.youtube.com/v/dQw4w9WgXcQ",
	] {
		assert_eq!(video::resolve_id(input).unwrap(), VIDEO, "{input}");
		// The livechat command takes a video id at its word, with no request.
		assert_eq!(source::resolve(input).unwrap().video_id, VIDEO, "{input}");
	}
}

#[test]
fn a_resolved_video_leads_back_to_itself() {
	let url = video::watch_url(VIDEO);
	assert_eq!(url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
	assert_eq!(video::resolve_id(&url).unwrap(), VIDEO);
	assert!(source::chat_url(VIDEO).contains(VIDEO));
}

#[test]
fn what_is_not_a_video_is_refused() {
	for input in [
		"",
		"   ",
		"@Someone",
		"https://www.youtube.com/@Someone",
		"https://www.youtube.com/watch?v=short",
		"dQw4w9WgXc!",
		"https://youtu.be/",
	] {
		assert!(video::resolve_id(input).is_err(), "{input:?}");
		assert!(!video::is_id(input.trim()), "{input:?}");
	}
}

#[test]
fn every_way_of_naming_a_channel_files_it_under_one_key() {
	for (input, key, page) in [
		("@Someone", "@Someone", "https://www.youtube.com/@Someone"),
		("Someone", "@Someone", "https://www.youtube.com/@Someone"),
		("https://www.youtube.com/@Someone/", "@Someone", "https://www.youtube.com/@Someone"),
		("https://www.youtube.com/@Someone/posts", "@Someone", "https://www.youtube.com/@Someone/posts"),
		("UCabcdefghijklmnopqrstuv", "UCabcdefghijklmnopqrstuv", "https://www.youtube.com/channel/UCabcdefghijklmnopqrstuv"),
		(
			"https://www.youtube.com/channel/UCabcdefghijklmnopqrstuv",
			"UCabcdefghijklmnopqrstuv",
			"https://www.youtube.com/channel/UCabcdefghijklmnopqrstuv",
		),
	] {
		let channel = Channel::parse(input);
		assert_eq!(channel.key().as_deref(), Some(key), "{input}");
		assert_eq!(channel.page_url(), page, "{input}");
		// The page leads back to the same key.
		assert_eq!(channel::key_from_url(&channel.page_url()).as_deref(), Some(key), "{input}");
	}
}

#[test]
fn a_channel_key_is_always_safe_as_a_file_name() {
	for input in ["@a/../../etc", "https://www.youtube.com/@we\\ird:name*", "@..", "@con?x"] {
		if let Some(key) = Channel::parse(input).key() {
			assert!(!key.contains(['/', '\\', ':', '*', '?']), "{input} -> {key}");
			assert!(key != "." && key != "..", "{input} -> {key}");
		}
	}
}

#[test]
fn a_post_page_gives_its_date_in_utc() {
	let html = r#"<script type="application/ld+json" nonce="x">{"datePublished":"2024-02-29T23:30:00-02:00","text":"leap"}</script>"#;
	let found = details::from_html(html).unwrap();
	assert_eq!(found.published_utc.as_deref(), Some("2024-03-01T01:30:00Z"));
	assert_eq!(found.text.as_deref(), Some("leap"));
	// The same instant, read straight.
	assert_eq!(time::to_utc("2024-03-01T01:30:00Z").unwrap(), "2024-03-01T01:30:00Z");
}
