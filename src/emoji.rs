//! Chat emoji, and the images of the custom ones, kept in one directory that
//! every run shares.
//!
//! YouTube marks every emoji in a message as a run of its own. A standard one
//! is a Unicode character, and a transcript writes it as that character, so
//! any font can draw it. A custom one, a channel's member emoji or one of
//! YouTube's own (`:yt:`, `:hand-pink-waving:`), is an image that no font has.
//! So a transcript writes its shortcut in the text, and lists it under `emojis`
//! with the URL of its image. With `--grab-emoji`, that image is also saved and
//! the list points at it as `image`.

use std::path::PathBuf;

use anyhow::Result;
use log::info;
use serde_json::{Value, json};

use crate::channel::sanitize;
use crate::image_store::ImageStore;

pub const DEFAULT_DIR: &str = "emojis";

/// The flag, for each extractor that sees emoji.
#[derive(clap::Args)]
#[group(id = "emoji")]
pub struct Args {
	/// Also save the image of every custom emoji in the output, into a
	/// directory shared by every run, and point each one at it as `image`. An
	/// image already there is reused rather than downloaded again. Takes the
	/// directory, defaulting to emojis.
	#[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = DEFAULT_DIR)]
	grab_emoji: Option<PathBuf>,
}

impl Args {
	/// The store the flag asks for, or `None` when it was not given.
	pub fn open(&self) -> Result<Option<ImageStore>> {
		let Some(dir) = &self.grab_emoji else {
			return Ok(None);
		};
		let store = ImageStore::create(dir, "emoji", false)?;
		info!("emojis:  {}", dir.display());
		Ok(Some(store))
	}
}

/// What one emoji run adds to a message's text. A custom emoji is also added
/// to `custom`, once per message however often it is used.
pub fn text(emoji: &Value, custom: &mut Vec<Value>) -> String {
	let id = emoji["emojiId"].as_str();
	if let Some(id) = id
		&& !is_custom(emoji)
	{
		return id.to_string();
	}
	// Each custom emoji names one or more shortcuts, the first being the one a
	// viewer sees on hover.
	let shortcut = emoji["shortcuts"][0].as_str().or(id).unwrap_or_default();
	if let Some(id) = id
		&& !custom.iter().any(|known| known["id"] == id)
	{
		custom.push(json!({
			"id": id,
			"shortcut": shortcut,
			// The largest size listed.
			"url": emoji["image"]["thumbnails"].as_array().and_then(|t| t.last()).and_then(|t| t["url"].as_str()),
		}));
	}
	shortcut.to_string()
}

/// Whether an emoji is an image rather than a character. YouTube says so with
/// `isCustomEmoji`; an id of the form `<channel>/<emoji>` says so too, for a
/// response that leaves the flag out.
fn is_custom(emoji: &Value) -> bool {
	emoji["isCustomEmoji"].as_bool().unwrap_or(false) || emoji["emojiId"].as_str().is_none_or(|id| id.contains('/'))
}

/// Adds `image` to each emoji in a transcript line's `emojis`, when images
/// were asked for: the saved file, or `null` when there is none.
pub fn attach_images(entry: &mut Value, store: Option<&ImageStore>) {
	let Some(store) = store else {
		return;
	};
	let Some(emojis) = entry["emojis"].as_array_mut() else {
		return;
	};
	for emoji in emojis {
		// The id is `<channel>/<emoji>`, which is one file name once the slash
		// is gone.
		let key = emoji["id"].as_str().and_then(|id| sanitize(&id.replace('/', "_")));
		let url = emoji["url"].as_str().map(str::to_string);
		let image = match (key, url) {
			(Some(key), Some(url)) => store.get(&key, || Ok(url)),
			_ => None,
		};
		emoji["image"] = image.into();
	}
}

/// One line on what the run added to the directory and what it found there.
pub fn report(store: &ImageStore) {
	let (saved, reused) = store.counts();
	info!("emojis: {saved} saved, {reused} reused, in {}", store.dir().display());
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn writes_a_standard_emoji_as_its_character() {
		let mut custom = Vec::new();
		let wave = json!({"emojiId": "\u{1f44b}", "shortcuts": [":waving_hand:"], "image": {"thumbnails": [{"url": "https://example.com/wave.svg"}]}});
		assert_eq!(text(&wave, &mut custom), "\u{1f44b}");
		// No image is needed for a character.
		assert!(custom.is_empty());
	}

	#[test]
	fn writes_a_custom_emoji_as_its_shortcut_and_lists_its_image() {
		let mut custom = Vec::new();
		let emote = json!({
			"emojiId": "UCabc/xyz",
			"shortcuts": [":_channelEmote:", ":channelEmote:"],
			"isCustomEmoji": true,
			"image": {"thumbnails": [{"url": "https://yt3.ggpht.com/e=w24-h24"}, {"url": "https://yt3.ggpht.com/e=w48-h48"}]},
		});
		assert_eq!(text(&emote, &mut custom), ":_channelEmote:");
		assert_eq!(text(&emote, &mut custom), ":_channelEmote:");
		// Listed once, however often it is used.
		assert_eq!(custom, vec![json!({"id": "UCabc/xyz", "shortcut": ":_channelEmote:", "url": "https://yt3.ggpht.com/e=w48-h48"})]);
	}

	#[test]
	fn takes_a_channel_id_as_a_custom_emoji_without_the_flag() {
		let mut custom = Vec::new();
		let yt = json!({"emojiId": "UCkszU2WH9gy1mb0dV-11UJg/abc", "shortcuts": [":yt:"]});
		assert_eq!(text(&yt, &mut custom), ":yt:");
		assert_eq!(custom.len(), 1);
		assert!(custom[0]["url"].is_null());
	}

	#[test]
	fn an_emoji_without_an_id_is_custom_and_written_by_its_shortcut() {
		let mut custom = Vec::new();
		assert_eq!(text(&json!({"shortcuts": [":odd:"]}), &mut custom), ":odd:");
		// With no id there is nothing to file its image under.
		assert!(custom.is_empty());
		assert_eq!(text(&json!({}), &mut custom), "");
	}

	#[test]
	fn a_custom_emoji_without_shortcuts_is_written_as_its_id() {
		let mut custom = Vec::new();
		assert_eq!(text(&json!({"emojiId": "UCabc/xyz"}), &mut custom), "UCabc/xyz");
		assert_eq!(custom[0]["shortcut"], "UCabc/xyz");
	}

	#[test]
	fn leaves_a_line_alone_when_images_were_not_asked_for() {
		let mut entry = json!({"emojis": [{"id": "UCabc/xyz", "url": "https://yt3.ggpht.com/e"}]});
		let before = entry.clone();
		attach_images(&mut entry, None);
		assert_eq!(entry, before);
	}

	#[test]
	fn points_each_custom_emoji_at_its_saved_image() {
		let dir = std::env::temp_dir().join(format!("yta-emoji-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("UCabc_xyz.png"), b"x").unwrap();

		let store = ImageStore::new(&dir, "emoji", false);
		let mut entry = json!({"emojis": [{"id": "UCabc/xyz", "shortcut": ":_channelEmote:", "url": "https://yt3.ggpht.com/e"}, {"shortcut": ":x:"}]});
		attach_images(&mut entry, Some(&store));
		assert!(entry["emojis"][0]["image"].as_str().is_some_and(|p| p.ends_with("/UCabc_xyz.png")));
		assert!(entry["emojis"][1]["image"].is_null());

		std::fs::remove_dir_all(&dir).unwrap();
	}
}
