//! Screenshots every community post on a YouTube channel, in YouTube's dark
//! theme, including the channel avatar, the full post text and any attached
//! image at full size.

mod browser;
mod posts;
mod published;

use std::path::PathBuf;

use anyhow::{Result, bail};

// const DEFAULT_CHANNEL: &str = "https://www.youtube.com/@DanielLarson-ob2jw/posts";
const DEFAULT_CHANNEL: &str = "https://www.youtube.com/@DanielLarson-ob2jw/posts";
const DEFAULT_OUT_DIR: &str = "screenshots";

const USAGE: &str = "\
Usage: yt-meta-archiver [--utc] [channel] [output-dir] [max-posts]

  channel      handle (@name), channel URL, or posts URL
  output-dir   screenshots are written to <output-dir>/<handle>
  max-posts    stop after this many posts

  --utc        look up each post's exact publication date. This costs one
               page fetch per post, so it is off by default; without it the
               index records only the relative time YouTube shows.

Pass \"\" for a positional argument to keep its default.";

fn main() -> Result<()> {
	let mut positional = Vec::new();
	let mut fetch_dates = false;

	for arg in std::env::args().skip(1) {
		match arg.trim() {
			"--utc" => fetch_dates = true,
			"-h" | "--help" => {
				println!("{USAGE}");
				return Ok(());
			}
			flag if flag.starts_with('-') => bail!("unknown flag {flag:?}\n\n{USAGE}"),
			// Empty positional arguments fall back to their defaults, so a
			// later argument can be given without repeating the earlier ones.
			value => positional.push(value.to_string()),
		}
	}

	let mut positional = positional.into_iter().map(|a| Some(a).filter(|a| !a.is_empty()));
	let channel = positional.next().flatten();
	let base_dir = positional.next().flatten();
	let limit = positional.next().flatten();

	let channel = channel.unwrap_or_else(|| DEFAULT_CHANNEL.to_string());
	let base_dir = PathBuf::from(base_dir.unwrap_or_else(|| DEFAULT_OUT_DIR.to_string()));
	let limit = limit.map(|a| a.parse()).transpose()?;

	let url = posts::posts_url(&channel);
	let out_dir = posts::out_dir_for(&url, &base_dir);
	println!("channel: {url}");
	println!("output:  {}", out_dir.display());
	if !fetch_dates {
		println!("dates:   relative only (pass --utc for exact dates)");
	}

	let written = posts::archive(&url, &out_dir, limit, fetch_dates)?;
	println!("\nwrote {written} screenshots to {}", out_dir.display());
	println!("index:   {}", out_dir.join(posts::INDEX_FILE).display());
	Ok(())
}
