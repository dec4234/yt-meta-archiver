//! The shared image directories, downloading from a local server: what is
//! saved, what it is named, and when a download is skipped.

mod common;

use common::{MockServer, Response, TempDir};
use serde_json::json;
use yt_meta_archiver::emoji;
use yt_meta_archiver::image_store::ImageStore;
use yt_meta_archiver::profile_picture::{self, ProfilePictures};

/// Serves `/<name>.<ext>` with the content type that extension implies, and
/// anything else as HTML.
fn images() -> MockServer {
	MockServer::start(|request| {
		let path = request.path();
		let content_type = match path.rsplit('.').next() {
			Some("jpg") => "image/jpeg",
			Some("png") => "image/png",
			Some("webp") => "image/webp; charset=binary",
			Some("gif") => "image/gif",
			_ if path.starts_with("/pp/") => "image/jpeg",
			_ if path == "/missing" => return Response::status(404),
			_ => "text/html",
		};
		Response::ok(content_type, format!("bytes of {path}"))
	})
}

#[test]
fn saves_an_image_named_by_its_content_type() {
	let server = images();
	let dir = TempDir::new("store-save");
	let store = ImageStore::new(dir.path(), "test image", false);

	let path = store.get("UCabc", || Ok(server.url("/served.webp"))).expect("a saved image");
	assert!(path.ends_with("/UCabc.webp"), "{path}");
	assert_eq!(std::fs::read_to_string(dir.path().join("UCabc.webp")).unwrap(), "bytes of /served.webp");
	assert_eq!(store.counts(), (1, 0));
}

#[test]
fn downloads_a_key_once_per_run_and_never_again_once_on_disk() {
	let server = images();
	let dir = TempDir::new("store-once");

	let store = ImageStore::new(dir.path(), "test image", false);
	let first = store.get("UCabc", || Ok(server.url("/a.png")));
	let again = store.get("UCabc", || panic!("asked again for a key this run already has"));
	assert_eq!(first, again);
	assert_eq!(server.requests().len(), 1);

	// A later run finds it on disk.
	let later = ImageStore::new(dir.path(), "test image", false);
	assert_eq!(later.get("UCabc", || Ok(server.url("/a.png"))), first);
	assert_eq!(server.requests().len(), 1);
	assert_eq!(later.counts(), (0, 1));
}

#[test]
fn a_refresh_replaces_an_image_even_in_another_format() {
	let server = images();
	let dir = TempDir::new("store-refresh");
	std::fs::write(dir.path().join("UCabc.jpg"), b"old").unwrap();

	let store = ImageStore::new(dir.path(), "test image", true);
	let path = store.get("UCabc", || Ok(server.url("/new.png"))).expect("a saved image");
	assert!(path.ends_with("/UCabc.png"), "{path}");
	// The old file is gone, or a later run could find it first.
	assert!(!dir.path().join("UCabc.jpg").exists());
	assert_eq!(store.counts(), (1, 0));
}

#[test]
fn something_that_is_not_an_image_is_not_saved() {
	let server = images();
	let dir = TempDir::new("store-not-image");
	let store = ImageStore::new(dir.path(), "test image", false);

	assert_eq!(store.get("page", || Ok(server.url("/page.html"))), None);
	assert_eq!(store.get("gone", || Ok(server.url("/missing"))), None);
	assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
	assert_eq!(store.counts(), (0, 0));
}

#[test]
fn downloads_a_profile_picture_at_the_shared_size() {
	let server = images();
	let dir = TempDir::new("pp-size");
	let pictures = ProfilePictures::new(ImageStore::new(dir.path(), "profile picture", false), profile_picture::SIZE);

	let photo = server.url("/pp/abc=s64-c-k-no-rj");
	let path = pictures.for_channel_id("UCabcdefghijklmnopqrstuv", Some(&photo)).expect("a picture");
	assert!(path.ends_with("/UCabcdefghijklmnopqrstuv.jpg"), "{path}");

	// Whatever size the source asked for, the shared size is what is fetched.
	let [request] = server.requests().try_into().unwrap();
	assert_eq!(request.path(), "/pp/abc=s176-c-k-no-rj");
}

#[test]
fn saves_custom_emoji_images_for_a_transcript_line() {
	let server = images();
	let dir = TempDir::new("emoji-save");
	let store = ImageStore::new(dir.path(), "emoji", false);

	let mut line = json!({"emojis": [
		{"id": "UCchan/one", "shortcut": ":one:", "url": server.url("/one.png")},
		{"id": "UCchan/two", "shortcut": ":two:", "url": server.url("/two.gif")},
		{"id": "UCchan/gone", "shortcut": ":gone:", "url": server.url("/missing")},
	]});
	emoji::attach_images(&mut line, Some(&store));

	assert!(line["emojis"][0]["image"].as_str().is_some_and(|p| p.ends_with("/UCchan_one.png")));
	assert!(line["emojis"][1]["image"].as_str().is_some_and(|p| p.ends_with("/UCchan_two.gif")));
	assert!(line["emojis"][2]["image"].is_null());
	assert_eq!(store.counts(), (2, 0));
}
