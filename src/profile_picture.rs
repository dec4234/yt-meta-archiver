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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow};
use log::{debug, info, warn};
use serde_json::Value;

use crate::http;

const DEFAULT_DIR: &str = "profile-pictures";

/// Pictures are fetched at this many pixels square, whatever size the URL we
/// were given asked for, so the same author is the same image wherever they
/// were found.
const SIZE: u32 = 176;

/// What a saved picture can be, in the order a directory is searched for one.
const EXTENSIONS: [&str; 4] = ["jpg", "png", "webp", "gif"];

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
		std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
		info!("avatars: {}", dir.display());
		Ok(Some(ProfilePictures::new(dir)))
	}
}

/// The shared directory, and what this run has already learned about it.
pub struct ProfilePictures {
	dir: PathBuf,
	/// Every author asked about so far, and the path recorded for them — `None`
	/// for one whose picture could not be had, so they are not tried again.
	known: Mutex<HashMap<String, Option<String>>>,
	saved: AtomicUsize,
	reused: AtomicUsize,
}

impl ProfilePictures {
	fn new(dir: &Path) -> Self {
		Self {
			dir: dir.to_path_buf(),
			known: Mutex::new(HashMap::new()),
			saved: AtomicUsize::new(0),
			reused: AtomicUsize::new(0),
		}
	}

	/// The picture of the channel at `channel_url`, read off its page.
	pub fn for_channel_url(&self, channel_url: &str) -> Option<String> {
		let key = key_from_url(channel_url)?;
		self.get(&key, || channel_picture_url(channel_url))
	}

	/// The picture of channel `channel_id`, from `photo_url` when the source
	/// gave one and from the channel's page when it did not.
	pub fn for_channel_id(&self, channel_id: &str, photo_url: Option<&str>) -> Option<String> {
		let key = sanitize(channel_id)?;
		self.get(&key, || match photo_url {
			Some(url) => Ok(url.to_string()),
			None => channel_picture_url(&format!("https://www.youtube.com/channel/{channel_id}")),
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
		info!(
			"profile pictures: {} saved, {} reused, in {}",
			self.saved.load(Ordering::Relaxed),
			self.reused.load(Ordering::Relaxed),
			self.dir.display()
		);
	}

	/// The recorded path of `key`'s picture: from memory if this run has asked
	/// already, from the directory if an earlier run saved it, and downloaded
	/// from wherever `locate` says otherwise.
	///
	/// Best effort: a picture that cannot be had costs only itself, and the
	/// entry records `null` for it.
	fn get(&self, key: &str, locate: impl FnOnce() -> Result<String>) -> Option<String> {
		if let Some(known) = self.known.lock().expect("profile picture lock poisoned").get(key) {
			return known.clone();
		}

		let found = match self.on_disk(key) {
			Some(path) => {
				self.reused.fetch_add(1, Ordering::Relaxed);
				Some(path)
			}
			None => match locate().and_then(|url| self.download(key, &url)) {
				Ok(path) => {
					self.saved.fetch_add(1, Ordering::Relaxed);
					debug!("saved profile picture {path}");
					Some(path)
				}
				Err(e) => {
					warn!("no profile picture for {key}: {e:#}");
					None
				}
			},
		};
		self.known.lock().expect("profile picture lock poisoned").insert(key.to_string(), found.clone());
		found
	}

	/// A picture an earlier run already saved for `key`.
	fn on_disk(&self, key: &str) -> Option<String> {
		EXTENSIONS
			.iter()
			.map(|ext| format!("{key}.{ext}"))
			.find(|file| self.dir.join(file).is_file())
			.map(|file| self.recorded(&file))
	}

	fn download(&self, key: &str, url: &str) -> Result<String> {
		let (bytes, content_type) = http::get_bytes(&resize(url))?;
		let ext = extension(content_type.as_deref()).ok_or_else(|| anyhow!("{url} is not an image ({})", content_type.as_deref().unwrap_or("no content type")))?;
		let file = format!("{key}.{ext}");
		let path = self.dir.join(&file);
		std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
		Ok(self.recorded(&file))
	}

	/// How an index refers to a picture: the shared directory and the file in
	/// it, with forward slashes whatever the platform.
	fn recorded(&self, file: &str) -> String {
		self.dir.join(file).to_string_lossy().replace('\\', "/")
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

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
	let from = text.find(start)? + start.len();
	let len = text[from..].find(end)?;
	Some(&text[from..from + len])
}

/// The key a channel URL files its picture under: the channel id from a
/// `/channel/UC…` URL, the `@handle` from a handle URL.
fn key_from_url(url: &str) -> Option<String> {
	let path = url.split(['?', '#']).next()?;
	let mut segments = path.split('/').skip_while(|s| !s.starts_with('@') && *s != "channel");
	let key = match segments.next()? {
		"channel" => segments.next()?,
		handle => handle,
	};
	sanitize(key)
}

/// A key reduced to what is safe in a file name.
fn sanitize(key: &str) -> Option<String> {
	let key: String = key.chars().filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '%')).collect();
	(!key.is_empty() && key != "." && key != "..").then_some(key)
}

/// `url` asking for a [`SIZE`] square picture. YouTube's avatar URLs carry
/// their size as an `=s88-…` or `=s88` suffix; one without it is left alone.
fn resize(url: &str) -> String {
	let Some(at) = url.rfind("=s") else {
		return url.to_string();
	};
	let digits = url[at + 2..].chars().take_while(char::is_ascii_digit).count();
	if digits == 0 {
		return url.to_string();
	}
	format!("{}=s{SIZE}{}", &url[..at], &url[at + 2 + digits..])
}

/// The file extension for an image's content type, or `None` for something
/// that is not one.
fn extension(content_type: Option<&str>) -> Option<&'static str> {
	let mime = content_type?.split(';').next()?.trim().to_ascii_lowercase();
	match mime.as_str() {
		"image/jpeg" | "image/jpg" => Some("jpg"),
		"image/png" => Some("png"),
		"image/webp" => Some("webp"),
		"image/gif" => Some("gif"),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn files_a_channel_under_its_id_or_handle() {
		assert_eq!(key_from_url("https://www.youtube.com/@Someone").as_deref(), Some("@Someone"));
		assert_eq!(key_from_url("https://www.youtube.com/@Someone/posts").as_deref(), Some("@Someone"));
		assert_eq!(key_from_url("https://www.youtube.com/channel/UC123abc?x=1").as_deref(), Some("UC123abc"));
		assert_eq!(key_from_url("https://www.youtube.com/watch?v=abc"), None);
	}

	#[test]
	fn keeps_only_what_is_safe_in_a_file_name() {
		assert_eq!(sanitize("@a/../b").as_deref(), Some("@a..b"));
		assert_eq!(sanitize("..").as_deref(), None);
		assert_eq!(sanitize("").as_deref(), None);
	}

	#[test]
	fn asks_for_one_size_whatever_the_url_did() {
		assert_eq!(resize("https://yt4.ggpht.com/abc=s64-c-k-c0x00ffffff-no-rj"), "https://yt4.ggpht.com/abc=s176-c-k-c0x00ffffff-no-rj");
		assert_eq!(resize("https://yt3.googleusercontent.com/abc=s900"), "https://yt3.googleusercontent.com/abc=s176");
		assert_eq!(resize("https://example.com/a.png"), "https://example.com/a.png");
	}

	#[test]
	fn names_images_by_their_content_type() {
		assert_eq!(extension(Some("image/jpeg")), Some("jpg"));
		assert_eq!(extension(Some("image/webp; charset=binary")), Some("webp"));
		assert_eq!(extension(Some("text/html")), None);
		assert_eq!(extension(None), None);
	}

	#[test]
	fn reuses_a_picture_already_on_disk() {
		let dir = std::env::temp_dir().join(format!("yta-pp-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("UCabc.jpg"), b"x").unwrap();

		let pictures = ProfilePictures::new(&dir);
		let found = pictures.for_channel_id("UCabc", None);
		assert!(found.is_some_and(|p| p.ends_with("/UCabc.jpg")));
		assert_eq!(pictures.reused.load(Ordering::Relaxed), 1);

		std::fs::remove_dir_all(&dir).unwrap();
	}
}
