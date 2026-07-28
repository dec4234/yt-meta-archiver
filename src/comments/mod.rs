//! The video-comments extractor: reads the comments under a single video and
//! writes a `comments.json` index of them, optionally screenshotting each one
//! in YouTube's dark theme.
//!
//! The comment markup is the same wherever comments hang, so [`meta`] and the
//! injected script are shared with the community-post extractor, which captures
//! the comments under a post as part of archiving it.

mod extract;
pub(crate) mod meta;

use std::path::PathBuf;

use anyhow::Result;
use log::info;

use crate::{browser, video};

/// The script injected into any page whose comments we are reading, shared with
/// the posts extractor.
pub(crate) const COMMENTS_JS: &str = include_str!("js/comments.js");

const DEFAULT_OUT_DIR: &str = "comments";

#[derive(clap::Args)]
pub struct Args {
	/// Video URL (watch, youtu.be, /live, /shorts or /embed link) or bare video id
	video: String,

	/// Keep only comments with at least this many likes. On a well-watched
	/// video this is the difference between a page of comments and thousands.
	#[arg(long, value_name = "LIKES", default_value_t = 0)]
	min_likes: u64,

	/// Stop after this many comments, counted after --min-likes has been
	/// applied. Loading stops as soon as that many are on the page, so it
	/// bounds the work as well as the output.
	#[arg(long, value_name = "COUNT")]
	max: Option<usize>,

	/// Also screenshot each kept comment, as the posts extractor does, and name
	/// the file in the index. Without it the run is metadata only — the page is
	/// still opened in a browser, there being no other way to read the comments.
	#[arg(long)]
	pic: bool,

	/// Write those screenshots as WebP instead of PNG, which is a fraction of
	/// the size at a quality that still reads cleanly. Takes the quality,
	/// 1-100, defaulting to 80.
	#[arg(long, value_name = "QUALITY", num_args = 0..=1, default_missing_value = "0", requires = "pic")]
	compress: Option<u32>,

	/// Index and screenshots are written to <OUTPUT_DIR>/<video id>
	#[arg(long, value_name = "OUTPUT_DIR", default_value = DEFAULT_OUT_DIR)]
	out: PathBuf,
}

pub fn run(args: &Args) -> Result<()> {
	let video_id = video::resolve_id(&args.video)?;
	let out_dir = args.out.join(&video_id);
	let format = browser::ImageFormat::from_arg(args.compress);

	info!("video:   {}", video::watch_url(&video_id));
	info!("output:  {}", out_dir.display());
	info!("index:   {}", out_dir.join(extract::INDEX_FILE).display());
	match args.min_likes {
		0 => info!("keeping: all top-level comments"),
		likes => info!("keeping: top-level comments with {likes}+ likes"),
	}
	if let Some(max) = args.max {
		info!("max:     {max} comments");
	}
	if args.pic {
		match format {
			browser::ImageFormat::Webp(quality) => info!("pics:    one screenshot per comment, webp at quality {quality}"),
			browser::ImageFormat::Png => info!("pics:    one screenshot per comment"),
		}
	}

	let opts = extract::Options {
		min_likes: args.min_likes,
		max: args.max,
		pics: args.pic,
		format,
	};
	let written = extract::extract(&video_id, &out_dir, &opts)?;
	info!("wrote {written} comments to {}", out_dir.join(extract::INDEX_FILE).display());
	Ok(())
}
