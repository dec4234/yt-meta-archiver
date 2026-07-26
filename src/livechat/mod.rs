//! The live chat extractor: follows a livestream's chat while it is happening
//! and writes every message to a JSONL transcript as it arrives, optionally
//! screenshotting each one in YouTube's dark theme.
//!
//! This is the one extractor with no end it can work towards — it archives a
//! stream as it goes, so it runs until the chat closes or you interrupt it, and
//! everything it has written is already on disk at any point.

mod source;
mod watch;

use std::path::PathBuf;

use anyhow::Result;
use log::info;

use crate::browser;

const DEFAULT_OUT_DIR: &str = "livechat";

#[derive(clap::Args)]
pub struct Args {
	/// Livestream URL or video id, or the channel (@handle or URL) whose
	/// current stream to follow
	stream: String,

	/// Also screenshot each message, as the posts extractor does
	#[arg(long)]
	pic: bool,

	/// Write those screenshots as WebP instead of PNG, which is a fraction of
	/// the size at a quality that still reads cleanly. Takes the quality,
	/// 1-100, defaulting to 80.
	#[arg(long, value_name = "QUALITY", num_args = 0..=1, default_missing_value = "0", requires = "pic")]
	compress: Option<u32>,

	/// Transcript and screenshots are written to <OUTPUT_DIR>/<video id>
	#[arg(long, value_name = "OUTPUT_DIR", default_value = DEFAULT_OUT_DIR)]
	out: PathBuf,
}

pub fn run(args: &Args) -> Result<()> {
	let stream = source::resolve(&args.stream)?;
	let out_dir = args.out.join(&stream.video_id);

	info!("stream:  {}", source::watch_url(&stream.video_id));
	if let Some(title) = &stream.title {
		info!("title:   {title}");
	}
	info!("output:  {}", out_dir.display());
	info!("chat:    {}", out_dir.join(watch::TRANSCRIPT_FILE).display());
	let format = browser::ImageFormat::from_arg(args.compress);
	if args.pic {
		match format {
			browser::ImageFormat::Webp(quality) => info!("pics:    one screenshot per message, webp at quality {quality}"),
			browser::ImageFormat::Png => info!("pics:    one screenshot per message"),
		}
	}
	info!("following the chat — interrupt with Ctrl-C to stop");

	let written = watch::watch(&stream, &out_dir, args.pic, format)?;
	info!("wrote {written} messages to {}", out_dir.join(watch::TRANSCRIPT_FILE).display());
	Ok(())
}
