//! The profile-picture grabber: saves the picture of each channel it is given
//! into the same shared directory that `--grab-profile-picture` fills, so a
//! picture saved here is reused by every other extractor and the other way
//! round.
//!
//! No browser is needed: a channel's picture is read off its page with a plain
//! fetch, which costs one page and one image per channel not yet on disk.

use std::path::PathBuf;

use anyhow::Result;
use log::info;

use crate::channel::{self, Channel};
use crate::image_store::ImageStore;
use crate::profile_picture::{self, ProfilePictures};

#[derive(clap::Args)]
pub struct Args {
	#[command(flatten)]
	list: channel::ListArgs,

	/// Download each picture at this many pixels square. YouTube serves up to
	/// 900. The other extractors use 176, and they reuse what is on disk at
	/// whatever size it was saved.
	#[arg(long, value_name = "PIXELS", default_value_t = profile_picture::SIZE, value_parser = clap::value_parser!(u32).range(16..=900))]
	size: u32,

	/// The shared directory the pictures go into
	#[arg(long, value_name = "DIR", default_value = profile_picture::DEFAULT_DIR)]
	out: PathBuf,
}

pub fn run(args: &Args) -> Result<()> {
	let store = ImageStore::create(&args.out, "profile picture", args.list.refresh)?;
	let pictures = ProfilePictures::new(store, args.size);
	info!("size:     {}px", args.size);

	channel::grab_each(&args.list, pictures.store(), |channel| match channel {
		Channel::Id(id) => pictures.for_channel_id(id, None),
		Channel::Url(url) => pictures.for_channel_url(url),
	})
}
