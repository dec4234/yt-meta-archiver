//! Reads back a chat transcript the archiver wrote and says what is in it: who
//! talked, how much, and when across the stream.
//!
//! The archiver only ever writes `chat.jsonl` — nothing has read one back, so a
//! finished archive could only be sized up by loading it into something else.
//! This is that missing half, as a dashboard in the shape of a training monitor:
//! a chart of message volume over the stream, a ranking beside it, and the
//! counts underneath.
//!
//! It renders three ways, because a tool that can only draw itself on a terminal
//! is a tool that cannot be piped or tested: the full [`tui`] dashboard on a tty,
//! the same numbers as flat text through a pipe, and a single dashboard frame
//! rendered off-screen for `--snapshot`.

mod record;
mod report;
mod stats;
mod timeline;
mod tui;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser)]
#[command(version, about = "Reads back a livestream chat transcript and says what is in it.")]
struct Args {
	/// The transcript to read: either `chat.jsonl` itself or the directory the
	/// archiver wrote it into.
	path: PathBuf,

	/// How many chatters to rank.
	#[arg(long, value_name = "N", default_value_t = 20)]
	top: usize,

	/// Time bucket for the activity chart: `30s`, `5m`, `1h`, or `auto` to pick
	/// one that gives the stream about forty bars.
	#[arg(long, value_name = "SPEC", default_value = "auto")]
	bucket: String,

	/// How many emoji shortcodes to list.
	#[arg(long, value_name = "N", default_value_t = 10)]
	emoji: usize,

	/// Bar width for the plain report, in columns. The dashboard sizes its own
	/// chart to the terminal.
	#[arg(long, value_name = "N", default_value_t = 50)]
	width: usize,

	/// Print the flat text report instead of the dashboard. This is also what
	/// happens on its own when output is not a terminal, so pipes and
	/// redirections keep working without the flag.
	#[arg(long)]
	plain: bool,

	/// Draw one dashboard frame at this size and print it as text, e.g.
	/// `--snapshot 120x40`. For seeing the dashboard where there is no terminal
	/// to run it in.
	#[arg(long, value_name = "COLSxROWS")]
	snapshot: Option<String>,
}

fn main() -> Result<()> {
	let args = Args::parse();

	let Some(bucket) = stats::parse_bucket_spec(&args.bucket) else {
		bail!("unrecognised bucket {:?} — try 30s, 5m, 1h, or auto", args.bucket);
	};

	let path = record::transcript_path(&args.path);
	let (records, failures) = record::read(&path)?;
	if records.is_empty() {
		bail!("{} has no messages in it", path.display());
	}
	let stats = stats::collect(&records, failures);

	// `auto` needs the span, which is only known once every message has been
	// placed, so it cannot be settled while parsing the command line.
	let bucket = bucket.unwrap_or_else(|| stats.span_ms().map_or(60_000, stats::auto_bucket));
	let title = path.display().to_string();

	if let Some(size) = &args.snapshot {
		let (cols, rows) = parse_size(size)?;
		print!("{}", tui::snapshot(&stats, &title, bucket, cols, rows)?);
		return Ok(());
	}

	// A dashboard writes escape sequences and takes over the screen, neither of
	// which a pipe wants.
	if args.plain || !std::io::stdout().is_terminal() {
		let buckets = stats::buckets(&stats.offsets, bucket);
		print!("{}", report::render(&stats, &title, &buckets, args.top, args.emoji, args.width));
		return Ok(());
	}

	tui::run(&stats, &title, bucket)
}

/// `120x40` -> `(120, 40)`.
fn parse_size(raw: &str) -> Result<(u16, u16)> {
	let (cols, rows) = raw.split_once(['x', 'X']).with_context(|| format!("unrecognised size {raw:?} — try 120x40"))?;
	let cols = cols.trim().parse().with_context(|| format!("unrecognised width in {raw:?}"))?;
	let rows = rows.trim().parse().with_context(|| format!("unrecognised height in {raw:?}"))?;
	Ok((cols, rows))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_snapshot_sizes() {
		assert_eq!(parse_size("120x40").unwrap(), (120, 40));
		assert_eq!(parse_size("60X20").unwrap(), (60, 20));
		assert!(parse_size("120").is_err());
		assert!(parse_size("widexhigh").is_err());
	}
}
