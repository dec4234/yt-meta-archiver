//! The banner grabber: saves the header image of each channel it is given into
//! a directory of its own, filed under the same keys as profile pictures.
//!
//! No browser is needed: the banner's URL is in the data of the channel's page,
//! read with a plain fetch. By default the original upload is kept, uncropped;
//! `--cropped` keeps the strip a desktop browser shows instead.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use log::{info, warn};

use crate::channel::{self, Channel};
use crate::http;
use crate::image_store::ImageStore;
use crate::profile_picture::between;

const DEFAULT_DIR: &str = "banners";

/// Where a channel page lists its banner in each size it serves, smallest
/// first. The first is the current header; the second is the older one, which
/// some pages still carry.
const SOURCE_LISTS: [&str; 2] = ["\"banner\":{\"imageBannerViewModel\":{\"image\":{\"sources\":[", "\"banner\":{\"thumbnails\":["];

#[derive(clap::Args)]
pub struct Args {
	#[command(flatten)]
	list: channel::ListArgs,

	/// Keep the banner as a desktop browser shows it, a wide strip cropped out
	/// of the upload, rather than the whole original image.
	#[arg(long)]
	cropped: bool,

	/// The directory the banners go into
	#[arg(long, value_name = "DIR", default_value = DEFAULT_DIR)]
	out: PathBuf,
}

pub fn run(args: &Args) -> Result<()> {
	let store = ImageStore::create(&args.out, "banner", args.list.refresh)?;
	info!(
		"banner:   {}",
		if args.cropped {
			"cropped, as on desktop"
		} else {
			"original upload"
		}
	);

	channel::grab_each(&args.list, &store, |channel| grab(&store, channel, args.cropped))
}

fn grab(store: &ImageStore, channel: &Channel, cropped: bool) -> Option<String> {
	let page = channel.page_url();
	let Some(key) = channel.key() else {
		warn!("no banner for {page}: not a /@handle or /channel/ URL");
		return None;
	};
	store.get(&key, || {
		let html = http::get_html(&page)?;
		banner_url(&html, cropped).ok_or_else(|| anyhow!("no banner on {page}"))
	})
}

/// The banner a channel page carries: the largest size it lists, or with
/// `cropped` unset, the original upload behind it.
fn banner_url(html: &str, cropped: bool) -> Option<String> {
	let list = SOURCE_LISTS.iter().find_map(|start| between(html, start, "]"))?;
	let largest = list.split("\"url\":\"").skip(1).filter_map(|rest| rest.split('"').next()).last()?;
	let url = largest.replace("\\u0026", "&");
	let url = if url.starts_with("//") {
		format!("https:{url}")
	} else {
		url
	};
	if !url.starts_with("http") {
		return None;
	}
	Some(if cropped {
		url
	} else {
		original(&url)
	})
}

/// `url` asking for the image as uploaded. A banner URL carries its size and
/// crop as a `=w2560-fcrop64=…` suffix after the image's own path; `=s0` in its
/// place asks for no resizing and no crop.
fn original(url: &str) -> String {
	let name_from = url.rfind('/').map_or(0, |i| i + 1);
	match url[name_from..].find('=') {
		Some(at) => format!("{}=s0", &url[..name_from + at]),
		None => format!("{url}=s0"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const PAGE: &str = r#"..."banner":{"imageBannerViewModel":{"image":{"sources":[{"url":"https://yt3.googleusercontent.com/abc=w1060-fcrop64=1,00005a57ffffa5a8-k-c0xffffffff-no-nd-rj","width":1060,"height":175},{"url":"https://yt3.googleusercontent.com/abc=w2560-fcrop64=1,00005a57ffffa5a8-k-c0xffffffff-no-nd-rj","width":2560,"height":424}]}}..."#;

	#[test]
	fn takes_the_largest_banner_listed() {
		assert_eq!(
			banner_url(PAGE, true).as_deref(),
			Some("https://yt3.googleusercontent.com/abc=w2560-fcrop64=1,00005a57ffffa5a8-k-c0xffffffff-no-nd-rj")
		);
	}

	#[test]
	fn asks_for_the_original_upload_by_default() {
		assert_eq!(banner_url(PAGE, false).as_deref(), Some("https://yt3.googleusercontent.com/abc=s0"));
	}

	#[test]
	fn reads_the_older_banner_list_too() {
		let page = r#"{"banner":{"thumbnails":[{"url":"//yt3.googleusercontent.com/old=w1060-fcrop64=1,00-k-no","width":1060}]}}"#;
		assert_eq!(banner_url(page, false).as_deref(), Some("https://yt3.googleusercontent.com/old=s0"));
	}

	#[test]
	fn a_page_without_a_banner_has_none() {
		assert_eq!(banner_url(r#"{"avatar":{"thumbnails":[]}}"#, false), None);
	}

	#[test]
	fn the_original_drops_the_size_and_crop() {
		assert_eq!(original("https://yt3.googleusercontent.com/a-b_c=w1060-fcrop64=1,00-k"), "https://yt3.googleusercontent.com/a-b_c=s0");
		assert_eq!(original("https://yt3.googleusercontent.com/abc"), "https://yt3.googleusercontent.com/abc=s0");
	}
}
