//! Archives what a YouTube channel has published: today the community posts,
//! captured as screenshots with a JSON index beside them.
//!
//! Each kind of thing we pull off a channel is its own module under a
//! subcommand of the same name; [`browser`], [`channel`], [`clip`], [`http`],
//! [`image_store`], [`profile_picture`], [`time`] and [`video`] are what they
//! share.

mod banner;
mod browser;
mod channel;
mod cli;
mod clip;
mod comments;
mod http;
mod image_store;
mod livechat;
mod pfp;
mod posts;
mod profile_picture;
mod time;
mod video;

use anyhow::Result;

fn main() -> Result<()> {
	cli::run()
}
