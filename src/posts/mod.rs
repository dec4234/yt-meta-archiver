//! The community-posts extractor: screenshots every post on a channel's posts
//! tab, in YouTube's dark theme, and writes a `posts.json` index beside them.

mod archive;
mod details;

use std::path::{Path, PathBuf};

use anyhow::Result;
use log::info;

use crate::{browser, cli};

const DEFAULT_CHANNEL: &str = "https://www.youtube.com/@DanielLarson-ob2jw/posts";
const DEFAULT_OUT_DIR: &str = "screenshots";

#[derive(clap::Args)]
#[command(after_help = "Pass \"\" for a positional argument to keep its default, so a later one \
                        can be given without repeating it.")]
pub struct Args {
	/// Handle (@name), channel URL, or posts URL
	#[arg(default_value = DEFAULT_CHANNEL)]
	channel: String,

	/// Screenshots are written to <OUTPUT_DIR>/<handle>
	#[arg(default_value = DEFAULT_OUT_DIR)]
	output_dir: String,

	/// Stop after this many posts
	max_posts: Option<usize>,

	/// Look up each post's exact publication date and text, from the post's own
	/// page. This costs one page fetch per post, so it is off by default;
	/// without it the index records only the relative time YouTube shows.
	#[arg(long, visible_alias = "utc")]
	details: bool,

	/// Write the screenshots as WebP instead of PNG, which is a fraction of the
	/// size at a quality that still reads cleanly. Takes the quality, 1-100,
	/// defaulting to 80.
	#[arg(long, value_name = "QUALITY", num_args = 0..=1, default_missing_value = "0")]
	compress: Option<u32>,

	/// Work on several posts at once: batches are captured in parallel browser
	/// sessions and --details lookups run side by side. Takes the number of
	/// workers, defaulting to one per core up to 4.
	#[arg(long, value_name = "WORKERS", num_args = 0..=1, default_missing_value = "0")]
	mt: Option<usize>,
}

pub fn run(args: &Args) -> Result<()> {
	// An empty positional falls back to its default, so a later argument can
	// be given without repeating the earlier ones.
	let channel = cli::non_empty(&args.channel).unwrap_or(DEFAULT_CHANNEL);
	let base_dir = PathBuf::from(cli::non_empty(&args.output_dir).unwrap_or(DEFAULT_OUT_DIR));

	let url = posts_url(channel);
	let out_dir = out_dir_for(&url, &base_dir);
	let workers = args.mt.map_or(1, cli::worker_count);
	let format = browser::ImageFormat::from_arg(args.compress);
	info!("channel: {url}");
	info!("output:  {}", out_dir.display());
	if !args.details {
		info!("details: relative dates only (pass --details for exact dates and post text)");
	}
	if let browser::ImageFormat::Webp(quality) = format {
		info!("images:  webp, quality {quality}");
	}
	if workers > 1 {
		info!("workers: {workers}");
	}

	let written = archive::archive(&url, &out_dir, args.max_posts, args.details, workers, format)?;
	info!("wrote {written} screenshots to {}", out_dir.display());
	info!("index:   {}", out_dir.join(archive::INDEX_FILE).display());
	Ok(())
}

/// `https://www.youtube.com/@handle/posts` from a handle, URL, or posts URL.
fn posts_url(input: &str) -> String {
	let input = input.trim().trim_end_matches('/');
	if input.starts_with("http") {
		if input.ends_with("/posts") {
			input.to_string()
		} else {
			format!("{input}/posts")
		}
	} else {
		let handle = input.strip_prefix('@').unwrap_or(input);
		format!("https://www.youtube.com/@{handle}/posts")
	}
}

/// A single post's page, which is where its [`details`] come from.
fn post_url(post_id: &str) -> String {
	format!("https://www.youtube.com/post/{post_id}")
}

/// Directory name derived from the channel handle, e.g. `screenshots/DanielLarson-ob2jw`.
fn out_dir_for(posts_url: &str, base: &Path) -> PathBuf {
	let handle = posts_url.split('/').find(|s| s.starts_with('@')).map(|s| s.trim_start_matches('@')).unwrap_or("channel");
	base.join(handle)
}
