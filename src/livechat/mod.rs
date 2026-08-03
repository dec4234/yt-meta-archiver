//! The live chat extractor: follows a livestream's chat while it is happening
//! and writes every message to a JSONL transcript as it arrives, optionally
//! screenshotting each one in YouTube's dark theme.
//!
//! A stream that is still going has no end this can work towards — it archives
//! the chat as it goes, so it runs until the chat closes or you interrupt it,
//! and everything it has written is already on disk at any point.
//!
//! A stream that is already over is the opposite, and a different job: YouTube
//! keeps its chat as a replay that can be read straight from the API, all of it,
//! far faster than the stream took to happen. So the same command covers both,
//! choosing between them on what the video turns out to be — [`watch`] follows a
//! live chat through a browser, [`replay`] reads a finished one over HTTP.

mod replay;
mod source;
mod watch;

use std::path::PathBuf;

use anyhow::Result;
use log::{info, warn};

use crate::{browser, video};

const DEFAULT_OUT_DIR: &str = "livechat";

#[derive(clap::Args)]
pub struct Args {
	/// Livestream URL or video id, or the channel (@handle or URL) whose
	/// current stream to follow. A stream that is already over is read from its
	/// chat replay instead, which takes seconds rather than hours.
	stream: String,

	/// Also screenshot each message, as the posts extractor does. Only a live
	/// chat can be photographed — a replay is read from the API, which has no
	/// page to point a camera at.
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
	let transcript = out_dir.join(watch::TRANSCRIPT_FILE);

	info!("stream:  {}", video::watch_url(&stream.video_id));
	if let Some(title) = &stream.title {
		info!("title:   {title}");
	}
	info!("output:  {}", out_dir.display());
	info!("chat:    {}", transcript.display());

	// Asked before a browser is launched, because the answer decides whether one
	// is needed at all.
	let chat = replay::probe(&stream.video_id).unwrap_or_else(|e| {
		// Following a live chat is what this command has always done, so a watch
		// page we could not read is no reason to stop it from trying.
		warn!("could not tell whether the stream is still live: {e:#}");
		replay::Chat::Unknown
	});

	let written = match chat {
		replay::Chat::Replay(replay) => {
			info!("replay:  the stream is over, reading its chat replay");
			if args.pic {
				warn!("--pic wants a rendered chat and a replay has none, so this run is the transcript only");
			}
			replay::archive(&replay, &out_dir)?
		}
		replay::Chat::Live | replay::Chat::Unknown => {
			let format = browser::ImageFormat::from_arg(args.compress);
			if args.pic {
				match format {
					browser::ImageFormat::Webp(quality) => info!("pics:    one screenshot per message, webp at quality {quality}"),
					browser::ImageFormat::Png => info!("pics:    one screenshot per message"),
				}
			}
			info!("following the chat — interrupt with Ctrl-C to stop");
			watch::watch(&stream, &out_dir, args.pic, format)?
		}
	};

	info!("wrote {written} messages to {}", transcript.display());
	Ok(())
}
