//! One line of a transcript, parsed.
//!
//! The archiver writes a chat as JSONL, one object per message, and the shape is
//! whatever `watch::entry` put there: a handful of fields always present, a few
//! that are present but null when the message has nothing to say, and a few that
//! are left out entirely rather than written as a wall of nulls. So every
//! optional here is one of those two cases, and the difference matters — an
//! absent `offset_ms` is what tells a live capture apart from a replay.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

/// The transcript file the archiver writes, so a directory can be given instead
/// of the file inside it.
pub const TRANSCRIPT_FILE: &str = "chat.jsonl";

/// One message.
pub struct Record {
	pub kind: String,
	pub author: Option<String>,
	pub author_channel_id: Option<String>,
	pub text: Option<String>,
	pub badges: Vec<String>,
	/// YouTube's own send time, second resolution. A live capture has nothing
	/// finer, and it is the only clock a live capture has at all.
	pub sent_utc: Option<String>,
	/// Milliseconds into the stream. Replay only, and negative for the chat that
	/// happened before the stream started.
	pub offset_ms: Option<i64>,
	/// Superchats and stickers only, as YouTube formatted it: `"£5.00"`.
	pub amount: Option<String>,
}

impl Record {
	/// Parses one line. `Ok(None)` for a blank one — the archiver flushes per
	/// message, so a trailing newline is normal rather than a defect.
	pub fn parse(line: &str) -> Result<Option<Record>> {
		if line.trim().is_empty() {
			return Ok(None);
		}
		let entry: Value = serde_json::from_str(line)?;
		Ok(Some(Record {
			// A line with no kind at all is still a message we counted; calling
			// it text is closer than dropping it.
			kind: entry["kind"].as_str().unwrap_or("text").to_string(),
			author: string(&entry["author"]),
			author_channel_id: string(&entry["author_channel_id"]),
			text: string(&entry["text"]),
			badges: entry["badges"].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(string).collect(),
			sent_utc: string(&entry["sent_utc"]),
			offset_ms: entry["offset_ms"].as_i64(),
			amount: string(&entry["amount"]),
		}))
	}

	/// The identity to group by. The channel id is the stable one — a handle can
	/// be changed, and two people can pick names that render alike — but a live
	/// capture reads it off the DOM and can come away without it, so the handle
	/// is the fallback.
	pub fn author_key(&self) -> String {
		self.author_channel_id.clone().or_else(|| self.author.clone()).unwrap_or_else(|| "(unknown)".to_string())
	}

	/// What to show for that identity: the handle a viewer would recognise, or
	/// the channel id when the handle is missing.
	pub fn display_name(&self) -> String {
		self.author.clone().or_else(|| self.author_channel_id.clone()).unwrap_or_else(|| "(unknown)".to_string())
	}
}

/// Every record in a transcript, and the number of lines that could not be
/// parsed. A bad line is not a reason to fail the run — the archiver appends
/// live and can be interrupted mid-write, so a truncated last line is an
/// ordinary thing to find, and the rest of the file is still worth reading.
pub fn read(path: &Path) -> Result<(Vec<Record>, usize)> {
	let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
	let mut records = Vec::new();
	let mut failures = 0;
	for line in BufReader::new(file).lines() {
		let line = line.with_context(|| format!("reading {}", path.display()))?;
		match Record::parse(&line) {
			Ok(Some(record)) => records.push(record),
			Ok(None) => {}
			Err(_) => failures += 1,
		}
	}
	Ok((records, failures))
}

/// The transcript a path names, whether it names the file or the directory the
/// archiver wrote it into.
pub fn transcript_path(path: &Path) -> PathBuf {
	if path.is_dir() {
		return path.join(TRANSCRIPT_FILE);
	}
	path.to_path_buf()
}

/// A JSON string, treating null and the empty string alike — an author YouTube
/// withheld comes through as one or the other depending on which path caught it.
fn string(value: &Value) -> Option<String> {
	let text = value.as_str()?;
	(!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_an_ordinary_text_line() {
		let line = r#"{"at":"2026-07-26T22:15:32Z","author":"@Hicham.h7q3o","author_channel_id":"UCUc0F6E2OZxlHORnBaT5zMA","badges":[],"id":"Chw","kind":"text","pic":"00001.webp","sent_utc":"2026-07-26T22:15:30Z","seq":1,"text":"hello"}"#;
		let record = Record::parse(line).unwrap().expect("a record");
		assert_eq!(record.kind, "text");
		assert_eq!(record.author.as_deref(), Some("@Hicham.h7q3o"));
		assert_eq!(record.text.as_deref(), Some("hello"));
		assert_eq!(record.author_key(), "UCUc0F6E2OZxlHORnBaT5zMA");
		assert_eq!(record.display_name(), "@Hicham.h7q3o");
		// Absent on a live capture, which is how the basis is chosen later.
		assert!(record.offset_ms.is_none());
		assert!(record.amount.is_none());
	}

	#[test]
	fn reads_a_replay_line_with_its_offset_and_amount() {
		let line = r#"{"seq":4,"id":"Chw","kind":"superchat","author":"@Payer","author_channel_id":"UCx","badges":["Member (2 years)"],"text":"do the wan show","sent_utc":"2026-03-14T00:11:29Z","offset_ms":-4200,"amount":"£5.00"}"#;
		let record = Record::parse(line).unwrap().expect("a record");
		assert_eq!(record.kind, "superchat");
		assert_eq!(record.amount.as_deref(), Some("£5.00"));
		assert_eq!(record.badges, vec!["Member (2 years)".to_string()]);
		// Chat starts before the stream does, so the offset can be negative.
		assert_eq!(record.offset_ms, Some(-4200));
	}

	#[test]
	fn falls_back_when_the_author_is_missing() {
		// A message YouTube withheld keeps its channel id but loses the handle.
		let record = Record::parse(r#"{"kind":"text","author":null,"author_channel_id":"UCx","badges":[],"text":null}"#)
			.unwrap()
			.expect("a record");
		assert!(record.author.is_none());
		assert!(record.text.is_none());
		assert_eq!(record.author_key(), "UCx");
		assert_eq!(record.display_name(), "UCx");

		// And with neither, there is still a bucket to count it in.
		let record = Record::parse(r#"{"kind":"text","author":null,"author_channel_id":null,"badges":[]}"#).unwrap().expect("a record");
		assert_eq!(record.author_key(), "(unknown)");
		assert_eq!(record.display_name(), "(unknown)");
	}

	#[test]
	fn blank_lines_are_not_records_and_junk_is_not_fatal() {
		assert!(Record::parse("").unwrap().is_none());
		assert!(Record::parse("   ").unwrap().is_none());
		// A run interrupted mid-write leaves a half-line behind.
		assert!(Record::parse(r#"{"kind":"text","author":"@Someo"#).is_err());
	}
}
