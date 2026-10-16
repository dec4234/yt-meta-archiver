//! Profile pictures of the people whose posts, comments and chat we archive,
//! kept in one directory that every extractor and every run shares.
//!
//! A picture is filed under who it belongs to — a channel id (`UC…`) where the
//! source names one, the `@handle` otherwise — so the same author seen again,
//! in this run or any later one, is a file that already exists rather than
//! another download. Index entries point at it by path under `profile_picture`.
//!
//! Where a source hands over the picture's URL (live chat does) it is used as
//! is; otherwise it is read off the author's channel page, which costs one page
//! fetch the first time an author is seen and nothing after that.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use log::{info, warn};
use serde_json::Value;

use crate::channel::{key_from_url, sanitize};
use crate::http;
use crate::image_store::ImageStore;

pub const DEFAULT_DIR: &str = "profile-pictures";

/// Pictures are fetched at this many pixels square, whatever size the URL we
/// were given asked for, so the same author is the same image wherever they
/// were found.
pub const SIZE: u32 = 176;

/// The flag, shared by every extractor.
#[derive(clap::Args)]
#[group(id = "profile_picture")]
pub struct Args {
	/// Also save the profile picture of every author in the output, into a
	/// directory shared by every tool and every run, and point each entry at it
	/// as `profile_picture`. A picture already there is reused rather than
	/// downloaded again. Takes the directory, defaulting to profile-pictures.
	#[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = DEFAULT_DIR)]
	grab_profile_picture: Option<PathBuf>,
}

impl Args {
	/// The store the flag asks for, or `None` when it was not given.
	pub fn open(&self) -> Result<Option<ProfilePictures>> {
		let Some(dir) = &self.grab_profile_picture else {
			return Ok(None);
		};
		let store = ImageStore::create(dir, "profile picture", false)?;
		info!("avatars: {}", dir.display());
		Ok(Some(ProfilePictures::new(store, SIZE)))
	}
}

/// The shared directory, and the size its pictures are downloaded at.
pub struct ProfilePictures {
	store: ImageStore,
	/// The square size, in pixels, a picture is downloaded at.
	size: u32,
}

impl ProfilePictures {
	pub fn new(store: ImageStore, size: u32) -> Self {
		Self {
			store,
			size,
		}
	}

	pub fn store(&self) -> &ImageStore {
		&self.store
	}

	/// The picture of the channel at `channel_url`, read off its page.
	pub fn for_channel_url(&self, channel_url: &str) -> Option<String> {
		let Some(key) = key_from_url(channel_url) else {
			warn!("no profile picture for {channel_url}: not a /@handle or /channel/ URL");
			return None;
		};
		self.store.get(&key, || channel_picture_url(channel_url).map(|url| resize(&url, self.size)))
	}

	/// The picture of channel `channel_id`, from `photo_url` when the source
	/// gave one and from the channel's page when it did not.
	pub fn for_channel_id(&self, channel_id: &str, photo_url: Option<&str>) -> Option<String> {
		let key = sanitize(channel_id)?;
		self.store.get(&key, || {
			let url = match photo_url {
				Some(url) => url.to_string(),
				None => channel_picture_url(&format!("https://www.youtube.com/channel/{channel_id}"))?,
			};
			Ok(resize(&url, self.size))
		})
	}

	/// Adds `profile_picture` to every comment in `entries`, and to every reply
	/// and comment list nested under them, by each one's `author_url`.
	pub fn attach_to_comments(&self, entries: &mut [Value]) {
		for entry in entries {
			let Some(object) = entry.as_object_mut() else {
				continue;
			};
			if let Some(url) = object.get("author_url").and_then(Value::as_str) {
				let picture = self.for_channel_url(url);
				object.insert("profile_picture".to_string(), picture.into());
			}
			for nested in ["subcomments", "comments"] {
				if let Some(Value::Array(list)) = object.get_mut(nested) {
					self.attach_to_comments(list);
				}
			}
		}
	}

	/// One line on what the run added to the directory and what it found there.
	pub fn report(&self) {
		let (saved, reused) = self.store.counts();
		info!("profile pictures: {saved} saved, {reused} reused, in {}", self.store.dir().display());
	}
}

/// The avatar URL a channel page carries. Its `og:image` is the avatar at full
/// size; the page data's own `avatar` is the fallback for a page without one.
fn channel_picture_url(channel_url: &str) -> Result<String> {
	let html = http::get_html(channel_url)?;
	between(&html, "<meta property=\"og:image\" content=\"", "\"")
		.or_else(|| between(&html, "\"avatar\":{\"thumbnails\":[{\"url\":\"", "\""))
		.map(|url| url.replace("&amp;", "&"))
		.filter(|url| url.starts_with("http"))
		.ok_or_else(|| anyhow!("no profile picture on {channel_url}"))
}

pub fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
	let from = text.find(start)? + start.len();
	let len = text[from..].find(end)?;
	Some(&text[from..from + len])
}

/// `url` asking for a `size` square picture. YouTube's avatar URLs carry
/// their size as an `=s88-…` or `=s88` suffix; one without it is left alone.
fn resize(url: &str, size: u32) -> String {
	let Some(at) = url.rfind("=s") else {
		return url.to_string();
	};
	let digits = url[at + 2..].chars().take_while(char::is_ascii_digit).count();
	if digits == 0 {
		return url.to_string();
	}
	format!("{}=s{size}{}", &url[..at], &url[at + 2 + digits..])
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn asks_for_one_size_whatever_the_url_did() {
		assert_eq!(
			resize("https://yt4.ggpht.com/abc=s64-c-k-c0x00ffffff-no-rj", SIZE),
			"https://yt4.ggpht.com/abc=s176-c-k-c0x00ffffff-no-rj"
		);
		assert_eq!(resize("https://yt3.googleusercontent.com/abc=s900", SIZE), "https://yt3.googleusercontent.com/abc=s176");
		assert_eq!(resize("https://yt3.googleusercontent.com/abc=s900", 800), "https://yt3.googleusercontent.com/abc=s800");
		assert_eq!(resize("https://example.com/a.png", SIZE), "https://example.com/a.png");
	}

	#[test]
	fn reuses_a_picture_already_on_disk() {
		let dir = std::env::temp_dir().join(format!("yta-pp-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("UCabc.jpg"), b"x").unwrap();

		let pictures = ProfilePictures::new(ImageStore::new(&dir, "profile picture", false), SIZE);
		let found = pictures.for_channel_id("UCabc", None);
		assert!(found.is_some_and(|p| p.ends_with("/UCabc.jpg")));
		assert_eq!(pictures.store().counts(), (0, 1));

		std::fs::remove_dir_all(&dir).unwrap();
	}
}
