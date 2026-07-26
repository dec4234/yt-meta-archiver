//! Archives what a YouTube channel has published: today the community posts,
//! captured as screenshots with a JSON index beside them.
//!
//! Each kind of thing we pull off a channel is its own module under a
//! subcommand of the same name; [`browser`], [`http`] and [`time`] are what
//! they share.

mod browser;
mod cli;
mod http;
mod posts;
mod time;

use anyhow::Result;

fn main() -> Result<()> {
	cli::run()
}
