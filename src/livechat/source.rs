//! Working out which livestream to watch.
//!
//! The user gives us either the stream itself or the channel airing it. A
//! stream URL carries its video id in the URL, so it costs nothing; a channel
//! has to be asked what it is currently live with, which is what
//! `youtube.com/@handle/live` answers — it redirects to the live watch page,
//! and the page it lands on names the video in its canonical link.

use anyhow::{Result, anyhow, bail};
use log::debug;

use crate::http;

/// A YouTube video id: 11 characters of the URL-safe base64 alphabet.
const ID_LEN: usize = 11;

/// The stream we are going to watch.
pub struct Stream {
	pub video_id: String,
	/// The stream's title, when the page we resolved through told us one.
	pub title: Option<String>,
}

/// Resolves whatever the user passed — a watch/live/youtu.be URL, a bare video
/// id, a @handle, or a channel URL — to the stream to archive.
///
/// A video id in the input is taken at its word and costs no request; anything
/// channel-shaped is resolved through the channel's `/live` page, which fails
/// with a clear message when the channel is not currently streaming.
pub fn resolve(input: &str) -> Result<Stream> {
	let input = input.trim();
	if input.is_empty() {
		bail!("no livestream or channel given");
	}

	if let Some(video_id) = video_id_from_url(input) {
		debug!("read video id {video_id} straight off the input");
		return Ok(Stream { video_id, title: None });
	}

	let url = live_url(input);
	debug!("asking {url} what is live");
	let html = http::get_html(&url)?;
	let video_id = canonical_video_id(&html)
		.ok_or_else(|| anyhow!("{input} is not currently live (no live video at {url})"))?;

	Ok(Stream {
		video_id,
		title: meta_content(&html, "title"),
	})
}

/// The popout live chat for a stream. The popout is a standalone page rather
/// than the iframe the watch page embeds, so it renders on its own.
pub fn chat_url(video_id: &str) -> String {
	format!("https://www.youtube.com/live_chat?v={video_id}&is_popout=1")
}

pub fn watch_url(video_id: &str) -> String {
	format!("https://www.youtube.com/watch?v={video_id}")
}

/// The video id an input names directly, if it names one: `watch?v=ID`,
/// `youtu.be/ID`, `/live/ID`, `/embed/ID`, or a bare id.
fn video_id_from_url(input: &str) -> Option<String> {
	if is_video_id(input) {
		return Some(input.to_string());
	}
	if !input.starts_with("http") {
		return None;
	}

	// Everything after the host, split into path segments and a query.
	let rest = input.split_once("//")?.1.split_once('/').map(|(_, rest)| rest).unwrap_or("");
	let (path, query) = rest.split_once('?').unwrap_or((rest, ""));

	if let Some(v) = query.split('&').find_map(|p| p.strip_prefix("v=")) {
		return is_video_id(v).then(|| v.to_string());
	}

	let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
	match segments.as_slice() {
		// youtu.be/ID — the id is the whole path.
		[id] if input.contains("youtu.be/") && is_video_id(id) => Some(id.to_string()),
		[kind, id, ..] if matches!(*kind, "live" | "embed" | "v") && is_video_id(id) => Some(id.to_string()),
		_ => None,
	}
}

/// `https://www.youtube.com/@handle/live` from a handle or channel URL.
fn live_url(input: &str) -> String {
	let input = input.trim_end_matches('/');
	if input.starts_with("http") {
		if input.ends_with("/live") {
			input.to_string()
		} else {
			format!("{input}/live")
		}
	} else {
		let handle = input.strip_prefix('@').unwrap_or(input);
		format!("https://www.youtube.com/@{handle}/live")
	}
}

/// The video id in a page's canonical link. A channel that is not live sends
/// its `/live` page to the channel itself, whose canonical is a channel URL —
/// so the absence of a video id here is exactly the "not live" answer.
fn canonical_video_id(html: &str) -> Option<String> {
	let tag = html.find("rel=\"canonical\"").and_then(|i| {
		let open = html[..i].rfind('<')?;
		let close = html[i..].find('>')? + i;
		Some(&html[open..close])
	})?;
	let href = attribute(tag, "href")?;
	video_id_from_url(href)
}

/// The `content` of a `<meta name="..">`, which is where the stream title is.
fn meta_content(html: &str, name: &str) -> Option<String> {
	let needle = format!("name=\"{name}\"");
	let i = html.find(&needle)?;
	let open = html[..i].rfind('<')?;
	let close = html[i..].find('>')? + i;
	attribute(&html[open..close], "content").map(unescape_entities).filter(|s| !s.is_empty())
}

/// A double-quoted attribute's value from a single tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
	let needle = format!("{name}=\"");
	let start = tag.find(&needle)? + needle.len();
	let len = tag[start..].find('"')?;
	Some(&tag[start..start + len])
}

/// The handful of entities YouTube escapes into attribute values.
fn unescape_entities(raw: &str) -> String {
	raw.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn is_video_id(s: &str) -> bool {
	s.len() == ID_LEN && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_video_ids_out_of_the_usual_urls() {
		let id = Some("dQw4w9WgXcQ".to_string());
		assert_eq!(video_id_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), id);
		assert_eq!(video_id_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s"), id);
		assert_eq!(video_id_from_url("https://youtu.be/dQw4w9WgXcQ"), id);
		assert_eq!(video_id_from_url("https://www.youtube.com/live/dQw4w9WgXcQ"), id);
		assert_eq!(video_id_from_url("https://www.youtube.com/embed/dQw4w9WgXcQ"), id);
		assert_eq!(video_id_from_url("dQw4w9WgXcQ"), id);
	}

	#[test]
	fn does_not_mistake_a_channel_for_a_video() {
		assert_eq!(video_id_from_url("https://www.youtube.com/@SomeChannel"), None);
		assert_eq!(video_id_from_url("https://www.youtube.com/@SomeChannel/live"), None);
		assert_eq!(video_id_from_url("@SomeChannel"), None);
		// Right shape, wrong length.
		assert_eq!(video_id_from_url("https://www.youtube.com/watch?v=tooshort"), None);
	}

	#[test]
	fn builds_live_urls_from_handles_and_channel_urls() {
		assert_eq!(live_url("@Someone"), "https://www.youtube.com/@Someone/live");
		assert_eq!(live_url("Someone"), "https://www.youtube.com/@Someone/live");
		assert_eq!(live_url("https://www.youtube.com/@Someone"), "https://www.youtube.com/@Someone/live");
		assert_eq!(live_url("https://www.youtube.com/@Someone/live"), "https://www.youtube.com/@Someone/live");
	}

	#[test]
	fn finds_the_live_video_on_a_channel_live_page() {
		let html = r#"<head><link rel="canonical" href="https://www.youtube.com/watch?v=dQw4w9WgXcQ"><meta name="title" content="Live now: it&#39;s happening"></head>"#;
		assert_eq!(canonical_video_id(html).as_deref(), Some("dQw4w9WgXcQ"));
		assert_eq!(meta_content(html, "title").as_deref(), Some("Live now: it's happening"));
	}

	#[test]
	fn reports_a_channel_that_is_not_live() {
		// What YouTube serves when /live falls back to the channel page.
		let html = r#"<link rel="canonical" href="https://www.youtube.com/@Someone">"#;
		assert_eq!(canonical_video_id(html), None);
		assert_eq!(canonical_video_id("<html></html>"), None);
	}
}
