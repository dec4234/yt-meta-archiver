//! Archives what a YouTube channel has published: community posts, comments,
//! live chat, profile pictures and banners.
//!
//! Each kind of thing we pull off a channel is its own module under a
//! subcommand of the same name; [`browser`], [`channel`], [`emoji`], [`http`],
//! [`image_store`], [`profile_picture`], [`time`] and [`video`] are what they
//! share. The binary is a thin wrapper over [`cli::run`]; the library exists so
//! that the tests under `tests/` can call the same functions it does.

pub mod banner;
pub mod browser;
pub mod channel;
pub mod cli;
mod clip;
pub mod comments;
pub mod emoji;
pub mod http;
pub mod image_store;
pub mod livechat;
pub mod pfp;
pub mod posts;
pub mod profile_picture;
pub mod time;
pub mod video;
