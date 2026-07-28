//! What the page says about one comment, and the index entry made of it.
//!
//! A comment renders the same under a community post as it does under a video,
//! so both extractors read it the same way; only the URL a comment lives at
//! differs, and that is the caller's to build.

use serde_json::Value;

/// What the page says about one comment, as read off the DOM.
#[derive(Clone, Default)]
pub(crate) struct Meta {
	pub id: Option<String>,
	pub author: Option<String>,
	pub author_url: Option<String>,
	pub text: Option<String>,
	/// Likes as a number, from the count YouTube renders.
	pub likes: u64,
	/// That count as shown, e.g. "1.2K", kept because it is what the
	/// screenshot has in it.
	pub likes_text: Option<String>,
	pub replies: u64,
	pub published: Option<String>,
	pub pinned: bool,
	pub hearted: bool,
}

impl Meta {
	pub(crate) fn from_value(value: &Value) -> Self {
		let string = |key: &str| value[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
		// The accessible label spells the count out in full where the visible
		// one is abbreviated, so it is the better of the two when present.
		let likes_text = string("likes");
		let likes = string("likesLabel").as_deref().map(parse_count).filter(|n| *n > 0);

		Self {
			id: string("id"),
			author: string("author"),
			author_url: string("authorUrl"),
			text: string("text"),
			likes: likes.unwrap_or_else(|| likes_text.as_deref().map_or(0, parse_count)),
			likes_text,
			replies: string("replies").as_deref().map_or(0, parse_count),
			published: string("published"),
			pinned: value["pinned"].as_bool().unwrap_or(false),
			hearted: value["hearted"].as_bool().unwrap_or(false),
		}
	}
}

/// One comment's index entry. `url` is where the comment itself lives, which
/// only the caller knows how to build, and `screenshot` is its capture's path
/// relative to the output directory, absent when there is none.
pub(crate) fn entry(meta: &Meta, url: Option<String>, screenshot: Option<&str>) -> Value {
	serde_json::json!({
		"url": url,
		"author": meta.author,
		"author_url": meta.author_url,
		"text": meta.text,
		"likes": meta.likes,
		"likes_text": meta.likes_text,
		"replies": meta.replies,
		"pinned": meta.pinned,
		"hearted": meta.hearted,
		"posted_relative": meta.published,
		"screenshot": screenshot,
	})
}

/// A YouTube id reduced to what is safe in a file name.
pub(crate) fn sanitize(id: Option<&str>) -> String {
	id.map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
		.filter(|s| !s.is_empty())
		.unwrap_or_else(|| "unknown".to_string())
}

/// A count as YouTube renders it — "1.2K", "1,234", "3 replies" — as a number.
/// Anything unrecognisable counts as zero, which for a like filter errs towards
/// leaving a comment out rather than capturing the whole feed.
pub(crate) fn parse_count(raw: &str) -> u64 {
	let Some(token) = raw.split_whitespace().next() else {
		return 0;
	};
	let multiplier = match token.chars().last().map(|c| c.to_ascii_uppercase()) {
		Some('K') => 1_000.0,
		Some('M') => 1_000_000.0,
		Some('B') => 1_000_000_000.0,
		_ => 1.0,
	};
	let digits: String = token.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
	digits.parse::<f64>().map_or(0, |n| (n * multiplier) as u64)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_the_counts_youtube_renders() {
		assert_eq!(parse_count("12"), 12);
		assert_eq!(parse_count(" 1,234 "), 1234);
		assert_eq!(parse_count("1.2K"), 1200);
		assert_eq!(parse_count("3.4M"), 3_400_000);
		assert_eq!(parse_count("2B"), 2_000_000_000);
		assert_eq!(parse_count("1,234 likes"), 1234);
		assert_eq!(parse_count("5 replies"), 5);
		assert_eq!(parse_count("1 reply"), 1);
	}

	#[test]
	fn unrecognisable_counts_are_zero() {
		assert_eq!(parse_count(""), 0);
		assert_eq!(parse_count("   "), 0);
		assert_eq!(parse_count("K"), 0);
		assert_eq!(parse_count("Reply"), 0);
	}

	#[test]
	fn prefers_the_exact_label_over_the_abbreviated_count() {
		let meta = Meta::from_value(&serde_json::json!({"likes": "1.2K", "likesLabel": "1,234 likes"}));
		assert_eq!(meta.likes, 1234);
		assert_eq!(meta.likes_text.as_deref(), Some("1.2K"));

		// No label, or one that says nothing: fall back to what is rendered.
		let meta = Meta::from_value(&serde_json::json!({"likes": "1.2K", "likesLabel": null}));
		assert_eq!(meta.likes, 1200);
	}

	#[test]
	fn an_unliked_comment_reads_as_zero() {
		let meta = Meta::from_value(&serde_json::json!({"likes": "", "likesLabel": null, "author": "Someone"}));
		assert_eq!(meta.likes, 0);
		assert_eq!(meta.likes_text, None);
		assert_eq!(meta.author.as_deref(), Some("Someone"));
	}

	#[test]
	fn keeps_only_what_is_safe_in_a_file_name() {
		assert_eq!(sanitize(Some("Ugkx/../Abc")), "UgkxAbc");
		assert_eq!(sanitize(Some("")), "unknown");
		assert_eq!(sanitize(None), "unknown");
	}
}
