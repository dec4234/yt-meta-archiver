//! The dashboard: the transcript drawn as a chart with the counts around it.
//!
//! Terminal setup is [`ratatui::init`], which enables raw mode, switches to the
//! alternate screen and installs a panic hook that restores both before
//! unwinding — the same arrangement a long-running monitor needs, and worth
//! having here for the same reason: a panic mid-render must not leave the
//! caller's shell with no echo and no cursor.

mod app;
mod panels;

use std::time::Duration;

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event};

use crate::stats::Stats;
use app::App;

/// How often the screen is redrawn when nothing is happening. The data never
/// changes under us — this only bounds how long a keypress waits.
const TICK: Duration = Duration::from_millis(100);

/// Runs the dashboard until it is quit.
pub fn run(stats: &Stats, title: &str, bucket_ms: i64) -> Result<()> {
	let mut terminal = ratatui::init();
	let result = event_loop(&mut terminal, stats, title, bucket_ms);
	// Unconditionally, so a failure inside the loop still hands the terminal
	// back in the state it was borrowed in.
	ratatui::restore();
	result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, stats: &Stats, title: &str, bucket_ms: i64) -> Result<()> {
	let mut app = App::new(stats, title, bucket_ms);
	while !app.quit {
		terminal.draw(|frame| panels::draw(frame, &mut app))?;
		if event::poll(TICK)?
			&& let Event::Key(key) = event::read()?
		{
			app.handle_key(key);
		}
	}
	Ok(())
}

/// One frame, drawn off-screen and returned as text.
///
/// The dashboard is otherwise only visible on a tty, which makes it impossible
/// to check in CI, over a pipe, or from anywhere without a terminal attached.
/// Rendering through the same widget code into a fixed-size buffer gives
/// something that can be read and diffed.
pub fn snapshot(stats: &Stats, title: &str, bucket_ms: i64, cols: u16, rows: u16) -> Result<String> {
	let mut terminal = Terminal::new(TestBackend::new(cols, rows))?;
	let mut app = App::new(stats, title, bucket_ms);
	terminal.draw(|frame| panels::draw(frame, &mut app))?;
	Ok(buffer_text(terminal.backend()))
}

/// The buffer's cells as plain lines.
///
/// `TestBackend`'s own `Display` quotes every row for use in assertions, which
/// is the wrong shape for something a person is meant to look at.
fn buffer_text(backend: &TestBackend) -> String {
	let buffer = backend.buffer();
	let mut out = String::new();
	for y in buffer.area.top()..buffer.area.bottom() {
		for x in buffer.area.left()..buffer.area.right() {
			out.push_str(buffer[(x, y)].symbol());
		}
		// Trailing blanks are just padding to the terminal's width.
		while out.ends_with(' ') {
			out.pop();
		}
		out.push('\n');
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::record::Record;
	use crate::stats;

	fn stats_from(lines: &[&str]) -> Stats {
		let records: Vec<Record> = lines.iter().map(|l| Record::parse(l).unwrap().expect("a record")).collect();
		stats::collect(&records, 0)
	}

	fn busy_stats() -> Stats {
		let records: Vec<Record> = (0..600)
			.map(|i| {
				let line = format!(
					r#"{{"kind":"text","author":"@chatter{}","author_channel_id":"UC{}","badges":[],"text":"hello :kiss:","offset_ms":{}}}"#,
					i % 9,
					i % 9,
					i * 1000
				);
				Record::parse(&line).unwrap().expect("a record")
			})
			.collect();
		stats::collect(&records, 0)
	}

	#[test]
	fn draws_every_panel() {
		let stats = busy_stats();
		let frame = snapshot(&stats, "livechat/x/chat.jsonl", 60_000, 120, 40).unwrap();

		assert!(frame.contains("Activity"));
		assert!(frame.contains("Top chatters"));
		assert!(frame.contains("Breakdown"));
		assert!(frame.contains("Controls"));
		assert!(frame.contains("Status"));
		// The status panel reports the totals and which clock produced them.
		assert!(frame.contains("600 messages"));
		assert!(frame.contains("replay offsets"));
		// And the chart drew something rather than leaving an empty box.
		assert!(frame.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)), "the braille plot is empty");
	}

	#[test]
	fn ranks_the_chatters_in_order() {
		let stats = stats_from(&[
			r#"{"kind":"text","author":"@loud","author_channel_id":"UCa","badges":[],"text":"a","offset_ms":0}"#,
			r#"{"kind":"text","author":"@loud","author_channel_id":"UCa","badges":[],"text":"b","offset_ms":1000}"#,
			r#"{"kind":"text","author":"@quiet","author_channel_id":"UCb","badges":[],"text":"c","offset_ms":2000}"#,
		]);
		let frame = snapshot(&stats, "chat.jsonl", 10_000, 120, 40).unwrap();
		let loud = frame.find("@loud").expect("the top chatter is listed");
		let quiet = frame.find("@quiet").expect("the second chatter is listed");
		assert!(loud < quiet, "the ranking is not in descending order");
	}

	#[test]
	fn says_so_when_there_is_no_timeline() {
		let stats = stats_from(&[r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"hello"}"#]);
		let frame = snapshot(&stats, "chat.jsonl", 60_000, 120, 40).unwrap();
		assert!(frame.contains("no timeline"));
		// Everything that does not need a clock still draws.
		assert!(frame.contains("Top chatters"));
		assert!(frame.contains("@a"));
	}

	#[test]
	fn leaves_out_the_rows_a_plain_chat_has_nothing_for() {
		let stats = busy_stats();
		let frame = snapshot(&stats, "chat.jsonl", 60_000, 120, 40).unwrap();
		// All one kind, no badges, no money — so none of those rows appear.
		assert!(!frame.contains("Kinds"));
		assert!(!frame.contains("Badges"));
		assert!(!frame.contains("Paid"));
		// The emoji it does have are shown.
		assert!(frame.contains(":kiss:"));
	}

	#[test]
	fn shows_the_rows_a_richer_chat_does_have() {
		let stats = stats_from(&[
			r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":["Moderator"],"text":"hi","offset_ms":0}"#,
			r#"{"kind":"superchat","author":"@b","author_channel_id":"UCb","badges":["Member (2 years)"],"text":"go on","amount":"£5.00","offset_ms":60000}"#,
		]);
		let frame = snapshot(&stats, "chat.jsonl", 30_000, 120, 40).unwrap();
		assert!(frame.contains("Kinds"));
		assert!(frame.contains("Badges"));
		assert!(frame.contains("Moderator"));
		assert!(frame.contains("Paid"));
	}

	#[test]
	fn survives_a_terminal_too_small_to_hold_it() {
		let stats = busy_stats();
		// Every panel cannot fit; the point is that it clips rather than panics.
		let frame = snapshot(&stats, "chat.jsonl", 60_000, 60, 20).unwrap();
		assert!(frame.contains("Activity"));
		// And an absurd size is still only a drawing problem.
		assert!(snapshot(&stats, "chat.jsonl", 60_000, 20, 6).is_ok());
	}
}
