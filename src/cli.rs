//! The command line: one subcommand per extractor, plus the flags they share.

use anyhow::Result;
use clap::{Parser, Subcommand};
use log::LevelFilter;
use simple_logger::SimpleLogger;

use crate::{comments, livechat, posts};

#[derive(Parser)]
#[command(version, about = "Archives what a YouTube channel has published.")]
struct Cli {
	#[command(subcommand)]
	command: Command,

	/// Log the per-step detail of the work; repeat (-vv) to include the
	/// browser's own protocol chatter. RUST_LOG, when set, overrides both.
	#[arg(short, long, global = true, action = clap::ArgAction::Count)]
	verbose: u8,
}

#[derive(Subcommand)]
enum Command {
	/// Screenshot a channel's community posts.
	Posts(posts::Args),

	/// Record a livestream's chat as it happens.
	Livechat(livechat::Args),

	/// Index a video's comments, optionally screenshotting them.
	Comments(comments::Args),
}

/// Parses the command line and runs whichever extractor it names.
pub fn run() -> Result<()> {
	let cli = Cli::parse();
	init_logging(cli.verbose)?;
	match &cli.command {
		Command::Posts(args) => posts::run(args),
		Command::Livechat(args) => livechat::run(args),
		Command::Comments(args) => comments::run(args),
	}
}

/// A positional argument the user actually gave, as opposed to `""` standing in
/// for its default.
pub fn non_empty(value: &str) -> Option<&str> {
	Some(value.trim()).filter(|v| !v.is_empty())
}

/// Workers for `--mt`. A bare `--mt` (or `--mt 0`) picks one per core, capped
/// at 4 — each worker is a whole browser, so more of them starts costing more
/// in memory and renderer contention than it buys back.
pub fn worker_count(requested: usize) -> usize {
	if requested > 0 {
		return requested;
	}
	std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 4)
}

/// Sets up logging. Our own output starts at info and gains the per-step
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
