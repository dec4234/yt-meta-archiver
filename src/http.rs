//! Plain page fetches, for the parts of YouTube that are readable without a
//! browser. Anything needing a rendered page goes through [`crate::browser`]
//! instead.

use anyhow::Result;
use log::trace;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// A YouTube page is around 750 KB and the data we want tends to sit near the
/// end of it, so the whole thing has to be read; this is only a guard against a
/// runaway response.
const MAX_PAGE_BYTES: u64 = 8 * 1024 * 1024;

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
