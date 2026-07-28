//! Naming a single video: the id an input carries, and the URLs built from it.
//!
//! Every extractor that works on one video starts here — the livestream to
//! follow, the video whose comments to read — so the shapes of URL a user may
//! reasonably paste are recognised in one place.

use anyhow::{Result, bail};

/// A YouTube video id: 11 characters of the URL-safe base64 alphabet.
const ID_LEN: usize = 11;

/// The video an input names, for a command that only accepts a video: a watch,
/// `youtu.be`, `/live`, `/shorts` or `/embed` URL, or a bare id.
pub fn resolve_id(input: &str) -> Result<String> {
	let input = input.trim();
	if input.is_empty() {
		bail!("no video given");
	}
	match id_from_url(input) {
		Some(id) => Ok(id),
		None => bail!("{input} does not name a video (want a watch URL, youtu.be link, or bare video id)"),
	}
}

pub fn watch_url(video_id: &str) -> String {
	format!("https://www.youtube.com/watch?v={video_id}")
}

/// The video id an input names directly, if it names one: `watch?v=ID`,
/// `youtu.be/ID`, `/live/ID`, `/shorts/ID`, `/embed/ID`, or a bare id.
///
/// A short is an ordinary video wearing a different player, so its id is worth
/// exactly as much here as any other: `watch?v=<short>` serves the same page,
/// comments and all.
pub fn id_from_url(input: &str) -> Option<String> {
	let input = input.trim();
	if is_id(input) {
		return Some(input.to_string());
	}
	if !input.starts_with("http") {
		return None;
	}

	// Everything after the host, split into path segments and a query.
	let rest = input.split_once("//")?.1.split_once('/').map(|(_, rest)| rest).unwrap_or("");
	let (path, query) = rest.split_once('?').unwrap_or((rest, ""));

	if let Some(v) = query.split('&').find_map(|p| p.strip_prefix("v=")) {
		return is_id(v).then(|| v.to_string());
	}

	let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
	match segments.as_slice() {
		// youtu.be/ID — the id is the whole path.
		[id] if input.contains("youtu.be/") && is_id(id) => Some(id.to_string()),
		[kind, id, ..] if matches!(*kind, "live" | "embed" | "shorts" | "v") && is_id(id) => Some(id.to_string()),
		_ => None,
	}
}

pub fn is_id(s: &str) -> bool {
	s.len() == ID_LEN && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_video_ids_out_of_the_usual_urls() {
		let id = Some("dQw4w9WgXcQ".to_string());
		assert_eq!(id_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), id);
		assert_eq!(id_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s"), id);
		assert_eq!(id_from_url("https://youtu.be/dQw4w9WgXcQ"), id);
		assert_eq!(id_from_url("https://www.youtube.com/live/dQw4w9WgXcQ"), id);
		assert_eq!(id_from_url("https://www.youtube.com/shorts/dQw4w9WgXcQ"), id);
		assert_eq!(id_from_url("https://www.youtube.com/shorts/dQw4w9WgXcQ?feature=share"), id);
		assert_eq!(id_from_url("https://www.youtube.com/embed/dQw4w9WgXcQ"), id);
		assert_eq!(id_from_url("dQw4w9WgXcQ"), id);
	}

	#[test]
	fn does_not_mistake_a_channel_for_a_video() {
		assert_eq!(id_from_url("https://www.youtube.com/@SomeChannel"), None);
		assert_eq!(id_from_url("https://www.youtube.com/@SomeChannel/live"), None);
		assert_eq!(id_from_url("@SomeChannel"), None);
		// Right shape, wrong length.
		assert_eq!(id_from_url("https://www.youtube.com/watch?v=tooshort"), None);
	}

	#[test]
	fn a_command_that_wants_a_video_says_so_when_given_a_channel() {
		assert_eq!(resolve_id(" dQw4w9WgXcQ ").unwrap(), "dQw4w9WgXcQ");
		assert!(resolve_id("@SomeChannel").is_err());
		assert!(resolve_id("").is_err());
	}
}
