//! Plain page fetches and API calls, for the parts of YouTube that are readable
//! without a browser. Anything needing a rendered page goes through
//! [`crate::browser`] instead.

use anyhow::Result;
use log::trace;
use serde_json::Value;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// A YouTube page is around 750 KB and the data we want tends to sit near the
/// end of it, so the whole thing has to be read; this is only a guard against a
/// runaway response.
const MAX_PAGE_BYTES: u64 = 8 * 1024 * 1024;

/// An original channel banner can be a 2560x1440 PNG of several megabytes;
/// this, again, only guards against a runaway response.
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Fetches `url` as English-language HTML, posing as a desktop browser —
/// YouTube serves a different page to anything that looks like a script.
pub fn get_html(url: &str) -> Result<String> {
	trace!("fetching {url}");
	let html = ureq::get(url)
		.header("user-agent", USER_AGENT)
		.header("accept-language", "en-US,en")
		.call()?
		.body_mut()
		.with_config()
		.limit(MAX_PAGE_BYTES)
		.read_to_string()?;

	trace!("read {} bytes from {url}", html.len());
	Ok(html)
}

/// Fetches `url` as raw bytes, along with the content type the server gave it,
/// for the images we keep rather than parse.
pub fn get_bytes(url: &str) -> Result<(Vec<u8>, Option<String>)> {
	trace!("fetching {url}");
	let mut response = ureq::get(url).header("user-agent", USER_AGENT).call()?;
	let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
	let bytes = response.body_mut().with_config().limit(MAX_IMAGE_BYTES).read_to_vec()?;

	trace!("read {} bytes from {url}", bytes.len());
	Ok((bytes, content_type))
}

/// Posts `body` as JSON and reads the JSON back. This is how YouTube's own
/// pages talk to their backend once they have loaded, so the endpoints that
/// paginate — live chat replay among them — are reachable without a browser.
pub fn post_json(url: &str, body: &Value) -> Result<Value> {
	trace!("posting to {url}");
	let text = ureq::post(url)
		.header("user-agent", USER_AGENT)
		.header("accept-language", "en-US,en")
		.header("content-type", "application/json")
		// The endpoint checks this against the referring page and answers a
		// bare 400 without it.
		.header("origin", "https://www.youtube.com")
		.send(serde_json::to_string(body)?)?
		.body_mut()
		.with_config()
		.limit(MAX_PAGE_BYTES)
		.read_to_string()?;

	trace!("read {} bytes from {url}", text.len());
	Ok(serde_json::from_str(&text)?)
}
