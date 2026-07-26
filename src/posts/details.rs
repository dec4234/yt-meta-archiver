//! Details a community post's own page carries but the feed does not.
//!
//! The feed only ever renders a relative time ("2 years ago"), and the post
//! text it shows is the screenshot's, not something we can read back. Each
//! post's page carries a schema.org `DiscussionForumPosting` block holding both
//! the real timestamp and the post's text verbatim, so both come from there —
//! reading the markup rather than OCRing the capture.

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::{http, time};

/// What a post's page tells us about it. Every field is optional: a lookup
/// that came back empty is still worth telling apart from one never made.
#[derive(Clone, Default)]
pub struct Details {
	/// When it went up, as an ISO 8601 UTC timestamp.
	pub published_utc: Option<String>,
	/// Its text, absent for a post that is only an attachment.
	pub text: Option<String>,
}

/// Fetches a post's page and reads its details off the JSON-LD block. Errors
/// only when the page cannot be fetched or carries no such block; a field the
/// block happens to omit comes back as `None`.
pub fn fetch(post_id: &str) -> Result<Details> {
	let html = http::get_html(&super::post_url(post_id))?;
	let posting = extract_posting(&html).ok_or_else(|| anyhow!("post page carried no JSON-LD block"))?;

	let published_utc = match posting["datePublished"].as_str() {
		Some(raw) => Some(time::to_utc(raw)?),
		None => None,
	};
	Ok(Details {
		published_utc,
		text: posting["text"].as_str().filter(|t| !t.is_empty()).map(str::to_string),
	})
}

/// The page's `DiscussionForumPosting`, parsed. Going through the JSON parser
/// rather than slicing the raw markup is what makes the text usable: it arrives
/// escaped, and a multi-line post is nothing but escapes.
fn extract_posting(html: &str) -> Option<Value> {
	const OPEN: &str = "<script type=\"application/ld+json\"";
	let rest = &html[html.find(OPEN)?..];
	let block = &rest[rest.find('>')? + 1..rest.find("</script>")?];
	serde_json::from_str(block).ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_the_json_ld_block() {
		// The shape YouTube serves, down to the nonce on the tag.
		let html = r#"<meta name="title" content="Post from Someone"><script type="application/ld+json" nonce="wO5HIMF6">{"@type":"https://schema.org/DiscussionForumPosting","url":"https://www.youtube.com/post/abc","datePublished":"2023-10-03T10:20:08.354091-07:00","text":"line one\nline two \"quoted\""}</script><script>var ytInitialData = {};</script>"#;
		let posting = extract_posting(html).unwrap();
		assert_eq!(posting["datePublished"], "2023-10-03T10:20:08.354091-07:00");
		assert_eq!(posting["text"], "line one\nline two \"quoted\"");
	}

	#[test]
	fn survives_a_page_without_one() {
		assert!(extract_posting("<html></html>").is_none());
		assert!(extract_posting(r#"<script type="application/ld+json">not json</script>"#).is_none());
	}
}
