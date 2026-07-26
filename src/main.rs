//! Screenshots every community post on a YouTube channel, in YouTube's dark
//! theme, including the channel avatar, the full post text and any attached
//! image at full size.

mod browser;
mod posts;
mod published;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use log::{LevelFilter, info};
use simple_logger::SimpleLogger;

const DEFAULT_CHANNEL: &str = "https://www.youtube.com/@DanielLarson-ob2jw/posts";
const DEFAULT_OUT_DIR: &str = "screenshots";

#[derive(Parser)]
#[command(
	version,
	about = "Screenshots every community post on a YouTube channel.",
	after_help = "Pass \"\" for a positional argument to keep its default, so a later one \
	              can be given without repeating it."
)]
struct Cli {
	/// Handle (@name), channel URL, or posts URL
	#[arg(default_value = DEFAULT_CHANNEL)]
	channel: String,

	/// Screenshots are written to <OUTPUT_DIR>/<handle>
	#[arg(default_value = DEFAULT_OUT_DIR)]
	output_dir: String,

	/// Stop after this many posts
	max_posts: Option<usize>,

	/// Look up each post's exact publication date. This costs one page fetch
	/// per post, so it is off by default; without it the index records only
	/// the relative time YouTube shows.
	#[arg(long)]
	utc: bool,

	/// Log the per-step detail of each capture; repeat (-vv) to include the
	/// browser's own protocol chatter. RUST_LOG, when set, overrides both.
	#[arg(short, long, action = clap::ArgAction::Count)]
	verbose: u8,
}

fn main() -> Result<()> {
	let cli = Cli::parse();
	init_logging(cli.verbose)?;

	// An empty positional falls back to its default, so a later argument can
	// be given without repeating the earlier ones.
	let channel = non_empty(&cli.channel).unwrap_or(DEFAULT_CHANNEL);
	let base_dir = PathBuf::from(non_empty(&cli.output_dir).unwrap_or(DEFAULT_OUT_DIR));

	let url = posts::posts_url(channel);
	let out_dir = posts::out_dir_for(&url, &base_dir);
	info!("channel: {url}");
	info!("output:  {}", out_dir.display());
	if !cli.utc {
		info!("dates:   relative only (pass --utc for exact dates)");
	}

	let written = posts::archive(&url, &out_dir, cli.max_posts, cli.utc)?;
	info!("wrote {written} screenshots to {}", out_dir.display());
	info!("index:   {}", out_dir.join(posts::INDEX_FILE).display());
	Ok(())
}

fn non_empty(value: &str) -> Option<&str> {
	Some(value.trim()).filter(|v| !v.is_empty())
}

/// Sets up logging. Our own output starts at info and gains the per-capture
/// detail at `-v`; the browser and HTTP crates are far noisier, so they stay
/// quiet until `-vv`. `RUST_LOG` overrides all of it.
fn init_logging(verbosity: u8) -> Result<()> {
	let (ours, theirs) = match verbosity {
		0 => (LevelFilter::Info, LevelFilter::Warn),
		1 => (LevelFilter::Debug, LevelFilter::Warn),
		_ => (LevelFilter::Trace, LevelFilter::Debug),
	};

	SimpleLogger::new()
		.with_level(theirs)
		.with_module_level(env!("CARGO_CRATE_NAME"), ours)
		.with_local_timestamps()
		.env()
		.init()?;
	Ok(())
}
