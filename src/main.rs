//! The `yt-meta-archiver` command. Everything it does lives in the library;
//! see [`yt_meta_archiver::cli`].

use anyhow::Result;

fn main() -> Result<()> {
	yt_meta_archiver::cli::run()
}
