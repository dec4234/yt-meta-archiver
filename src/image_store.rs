//! A directory of images filed by who they belong to, shared by every run.
//!
//! Each image is saved as `<key>.<ext>`, where the key names its owner (see
//! [`crate::channel`]) and the extension follows the content type it was
//! served with. An image already in the directory is reused rather than
//! downloaded again, so a key seen again, in this run or any later one, costs
//! nothing. Profile pictures and banners are each kept in a store of their own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow};
use log::{debug, warn};

use crate::http;

/// What a saved image can be, in the order a directory is searched for one.
const EXTENSIONS: [&str; 4] = ["jpg", "png", "webp", "gif"];

/// The directory, and what this run has already learned about it.
pub struct ImageStore {
	dir: PathBuf,
	/// What the images are, for the log: "profile picture", "banner".
	what: &'static str,
	/// Download every image again, over whatever an earlier run saved.
	refresh: bool,
	/// Every key asked about so far, and the path recorded for it — `None` for
	/// one whose image could not be had, so it is not tried again.
	known: Mutex<HashMap<String, Option<String>>>,
	saved: AtomicUsize,
	reused: AtomicUsize,
}

impl ImageStore {
	/// A store over `dir`, which must already exist. With `refresh` set, the
	/// images already in it are ignored and replaced.
	pub fn new(dir: &Path, what: &'static str, refresh: bool) -> Self {
		Self {
			dir: dir.to_path_buf(),
			what,
			refresh,
			known: Mutex::new(HashMap::new()),
			saved: AtomicUsize::new(0),
			reused: AtomicUsize::new(0),
		}
	}

	/// Creates `dir` if it is missing, then opens a store over it.
	pub fn create(dir: &Path, what: &'static str, refresh: bool) -> Result<Self> {
		std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
		Ok(Self::new(dir, what, refresh))
	}

	pub fn dir(&self) -> &Path {
		&self.dir
	}

	pub fn what(&self) -> &'static str {
		self.what
	}

	/// How many images this run downloaded, and how many it found on disk.
	pub fn counts(&self) -> (usize, usize) {
		(self.saved.load(Ordering::Relaxed), self.reused.load(Ordering::Relaxed))
	}

	/// The recorded path of `key`'s image: from memory if this run has asked
	/// already, from the directory if an earlier run saved it, and downloaded
	/// from the URL `locate` gives otherwise.
	///
	/// Best effort: an image that cannot be had costs only itself, and the
	/// caller records `None` for it.
	pub fn get(&self, key: &str, locate: impl FnOnce() -> Result<String>) -> Option<String> {
		if let Some(known) = self.known.lock().expect("image store lock poisoned").get(key) {
			return known.clone();
		}

		let on_disk = if self.refresh {
			None
		} else {
			self.on_disk(key)
		};
		let found = match on_disk {
			Some(path) => {
				self.reused.fetch_add(1, Ordering::Relaxed);
				Some(path)
			}
			None => match locate().and_then(|url| self.download(key, &url)) {
				Ok(path) => {
					self.saved.fetch_add(1, Ordering::Relaxed);
					debug!("saved {} {path}", self.what);
					Some(path)
				}
				Err(e) => {
					warn!("no {} for {key}: {e:#}", self.what);
					None
				}
			},
		};
		self.known.lock().expect("image store lock poisoned").insert(key.to_string(), found.clone());
		found
	}

	/// An image an earlier run already saved for `key`.
	fn on_disk(&self, key: &str) -> Option<String> {
		EXTENSIONS
			.iter()
			.map(|ext| format!("{key}.{ext}"))
			.find(|file| self.dir.join(file).is_file())
			.map(|file| self.recorded(&file))
	}

	fn download(&self, key: &str, url: &str) -> Result<String> {
		let (bytes, content_type) = http::get_bytes(url)?;
		let ext = extension(content_type.as_deref()).ok_or_else(|| anyhow!("{url} is not an image ({})", content_type.as_deref().unwrap_or("no content type")))?;
		// A refreshed image can come back in another format than the one it
		// replaces; leave only the new one, or a later run could find the old.
		for old in EXTENSIONS.iter().filter(|&&old| old != ext) {
			let _ = std::fs::remove_file(self.dir.join(format!("{key}.{old}")));
		}
		let file = format!("{key}.{ext}");
		let path = self.dir.join(&file);
		std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
		Ok(self.recorded(&file))
	}

	/// How an index refers to an image: the shared directory and the file in
	/// it, with forward slashes whatever the platform.
	fn recorded(&self, file: &str) -> String {
		self.dir.join(file).to_string_lossy().replace('\\', "/")
	}
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
	fn names_images_by_their_content_type() {
		assert_eq!(extension(Some("image/jpeg")), Some("jpg"));
		assert_eq!(extension(Some("image/webp; charset=binary")), Some("webp"));
		assert_eq!(extension(Some("text/html")), None);
		assert_eq!(extension(None), None);
	}

	#[test]
	fn reads_content_types_in_any_case_and_both_jpeg_spellings() {
		assert_eq!(extension(Some("IMAGE/PNG")), Some("png"));
		assert_eq!(extension(Some(" image/gif ")), Some("gif"));
		assert_eq!(extension(Some("image/jpg")), Some("jpg"));
		assert_eq!(extension(Some("image/svg+xml")), None);
		assert_eq!(extension(Some("")), None);
	}

	#[test]
	fn creates_its_directory() {
		let dir = std::env::temp_dir().join(format!("yta-store-create-test-{}", std::process::id())).join("nested");
		let store = ImageStore::create(&dir, "test image", false).unwrap();
		assert!(dir.is_dir());
		assert_eq!(store.dir(), dir.as_path());
		assert_eq!(store.what(), "test image");
		std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
	}

	#[test]
	fn remembers_a_key_it_could_not_get_and_does_not_ask_again() {
		let dir = std::env::temp_dir().join(format!("yta-store-miss-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let store = ImageStore::new(&dir, "test image", false);
		let asked = std::cell::Cell::new(0);
		let locate = || {
			asked.set(asked.get() + 1);
			Err(anyhow!("offline"))
		};
		assert_eq!(store.get("UCmissing", locate), None);
		assert_eq!(store.get("UCmissing", locate), None);
		assert_eq!(asked.get(), 1);
		std::fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn finds_an_image_saved_in_any_known_format() {
		let dir = std::env::temp_dir().join(format!("yta-store-ext-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("a.webp"), b"x").unwrap();
		std::fs::write(dir.join("b.gif"), b"x").unwrap();
		let store = ImageStore::new(&dir, "test image", false);
		assert!(store.get("a", || Err(anyhow!("offline"))).is_some_and(|p| p.ends_with("/a.webp")));
		assert!(store.get("b", || Err(anyhow!("offline"))).is_some_and(|p| p.ends_with("/b.gif")));
		// A paths is recorded with forward slashes on every platform.
		assert!(!store.get("a", || Err(anyhow!("offline"))).unwrap().contains('\\'));
		std::fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn reuses_an_image_already_on_disk() {
		let dir = std::env::temp_dir().join(format!("yta-store-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("UCabc.jpg"), b"x").unwrap();

		let store = ImageStore::new(&dir, "test image", false);
		let found = store.get("UCabc", || Err(anyhow!("offline")));
		assert!(found.is_some_and(|p| p.ends_with("/UCabc.jpg")));
		assert_eq!(store.counts(), (0, 1));

		std::fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn a_refresh_ignores_an_image_already_on_disk() {
		let dir = std::env::temp_dir().join(format!("yta-store-refresh-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("UCabc.jpg"), b"x").unwrap();

		let store = ImageStore::new(&dir, "test image", true);
		let found = store.get("UCabc", || Err(anyhow!("offline")));
		assert_eq!(found, None);
		assert_eq!(store.counts(), (0, 0));

		std::fs::remove_dir_all(&dir).unwrap();
	}
}
