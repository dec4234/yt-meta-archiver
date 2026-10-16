//! Archives what a YouTube channel has published: today the community posts,
//! captured as screenshots with a JSON index beside them.
//!
//! Each kind of thing we pull off a channel is its own module under a
//! subcommand of the same name; [`browser`], [`clip`], [`http`],
//! [`profile_picture`], [`time`] and [`video`] are what they share.

mod browser;
mod cli;
mod clip;
mod comments;
mod http;
mod livechat;
mod posts;
mod profile_picture;
mod time;
mod video;

use anyhow::Result;

fn main() -> Result<()> {
	cli::run()
}
