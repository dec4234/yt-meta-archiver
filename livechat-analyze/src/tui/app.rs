//! What the dashboard is showing, and what the keys do to it.
//!
//! Every message's offset is already in memory, so re-bucketing is a walk over a
//! `Vec<i64>` rather than a re-read of the transcript. That is the whole reason
//! the bucket size is a key rather than only a flag: on a fourteen-hour stream
//! the difference between 5m and 30m bars is the difference between seeing the
//! texture and seeing the shape, and you do not know which you want until you
//! have looked.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::widgets::TableState;

use crate::stats::{self, Buckets, Stats};

pub struct App<'a> {
	pub stats: &'a Stats,
	pub title: &'a str,
	pub bucket_ms: i64,
	pub buckets: Buckets,
	/// Which bucket the cursor is on, and so which one the gauge describes.
	pub scrub: usize,
	pub table: TableState,
	pub quit: bool,
}

impl<'a> App<'a> {
	pub fn new(stats: &'a Stats, title: &'a str, bucket_ms: i64) -> Self {
		let buckets = stats::buckets(&stats.offsets, bucket_ms);
		// Opening on the busiest moment, because that is what anyone looking at
		// a chat's shape wants to find first.
		let scrub = buckets.peak_index();
		let mut table = TableState::default();
		table.select(Some(0));
		App {
			stats,
			title,
			bucket_ms,
			buckets,
			scrub,
			table,
			quit: false,
		}
	}

	/// Redraws the buckets at a new size, keeping the cursor over the same
	/// moment in the stream rather than the same index — the indices mean
	/// something different after a resize.
	fn rebucket(&mut self, coarser: bool) {
		let was = self.buckets.start_of(self.scrub);
		let size = stats::step_bucket(self.bucket_ms, coarser);
		if size == self.bucket_ms {
			return;
		}
		self.bucket_ms = size;
		self.buckets = stats::buckets(&self.stats.offsets, size);
		self.scrub = self.bucket_for(was);
	}

	/// The bucket holding a given moment, clamped to the ones that exist.
	fn bucket_for(&self, at_ms: i64) -> usize {
		if self.buckets.counts.is_empty() {
			return 0;
		}
		let index = (at_ms - self.buckets.start_ms).max(0) / self.buckets.size_ms;
		(index as usize).min(self.buckets.counts.len() - 1)
	}

	fn scrub_by(&mut self, delta: isize) {
		if self.buckets.counts.is_empty() {
			return;
		}
		let last = self.buckets.counts.len() - 1;
		self.scrub = self.scrub.saturating_add_signed(delta).min(last);
	}

	fn select_by(&mut self, delta: isize) {
		if self.stats.authors.is_empty() {
			return;
		}
		let last = self.stats.authors.len() - 1;
		let current = self.table.selected().unwrap_or(0);
		self.table.select(Some(current.saturating_add_signed(delta).min(last)));
	}

	pub fn handle_key(&mut self, key: KeyEvent) {
		// A key that is only being released is not a keypress; without this,
		// terminals that report both act on everything twice.
		if key.kind != KeyEventKind::Press {
			return;
		}
		match key.code {
			KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
			KeyCode::Left | KeyCode::Char('h') => self.scrub_by(-1),
			KeyCode::Right | KeyCode::Char('l') => self.scrub_by(1),
			KeyCode::Home => self.scrub = 0,
			KeyCode::End => self.scrub = self.buckets.counts.len().saturating_sub(1),
			KeyCode::Char('p') => self.scrub = self.buckets.peak_index(),
			KeyCode::Up | KeyCode::Char('k') => self.select_by(-1),
			KeyCode::Down | KeyCode::Char('j') => self.select_by(1),
			KeyCode::PageUp => self.select_by(-10),
			KeyCode::PageDown => self.select_by(10),
			// `=` because it is the unshifted `+` on most layouts, and asking
			// for shift to widen a bar is a needless thing to ask for.
			KeyCode::Char('+') | KeyCode::Char('=') => self.rebucket(true),
			KeyCode::Char('-') | KeyCode::Char('_') => self.rebucket(false),
			_ => {}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::record::Record;

	fn stats_of(count: i64) -> Stats {
		let records: Vec<Record> = (0..count)
			.map(|i| {
				Record::parse(&format!(
					r#"{{"kind":"text","author":"@a{}","author_channel_id":"UC{}","badges":[],"text":"hi","offset_ms":{}}}"#,
					i % 7,
					i % 7,
					i * 1000
				))
				.unwrap()
				.expect("a record")
			})
			.collect();
		crate::stats::collect(&records, 0)
	}

	fn press(app: &mut App, code: KeyCode) {
		app.handle_key(KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE));
	}

	#[test]
	fn opens_on_the_busiest_bucket() {
		let stats = stats_of(600);
		let app = App::new(&stats, "chat.jsonl", 60_000);
		assert_eq!(app.scrub, app.buckets.peak_index());
	}

	#[test]
	fn scrubbing_stops_at_the_ends() {
		let stats = stats_of(600);
		let mut app = App::new(&stats, "chat.jsonl", 60_000);
		press(&mut app, KeyCode::Home);
		assert_eq!(app.scrub, 0);
		press(&mut app, KeyCode::Left);
		assert_eq!(app.scrub, 0, "cannot scrub before the first bucket");
		press(&mut app, KeyCode::End);
		assert_eq!(app.scrub, app.buckets.counts.len() - 1);
		press(&mut app, KeyCode::Right);
		assert_eq!(app.scrub, app.buckets.counts.len() - 1, "cannot scrub past the last");
	}

	#[test]
	fn rebucketing_keeps_the_cursor_over_the_same_moment() {
		let stats = stats_of(600);
		let mut app = App::new(&stats, "chat.jsonl", 60_000);
		press(&mut app, KeyCode::Home);
		press(&mut app, KeyCode::Right);
		press(&mut app, KeyCode::Right);
		let was = app.buckets.start_of(app.scrub);

		press(&mut app, KeyCode::Char('+'));
		assert_eq!(app.bucket_ms, 300_000);
		// The index changed, but the moment under the cursor did not.
		let now = app.buckets.start_of(app.scrub);
		assert!(now <= was && was < now + app.bucket_ms, "{was} should be inside the bucket starting {now}");

		// And the counts still add up at the new size.
		assert_eq!(app.buckets.counts.iter().sum::<usize>(), 600);
	}

	#[test]
	fn rebucketing_stops_at_the_ends_of_the_ladder() {
		let stats = stats_of(600);
		let mut app = App::new(&stats, "chat.jsonl", 60_000);
		for _ in 0..10 {
			press(&mut app, KeyCode::Char('-'));
		}
		assert_eq!(app.bucket_ms, 10_000);
		for _ in 0..10 {
			press(&mut app, KeyCode::Char('='));
		}
		assert_eq!(app.bucket_ms, 3_600_000);
		assert!(!app.buckets.counts.is_empty());
	}

	#[test]
	fn the_ranking_scrolls_within_its_bounds() {
		let stats = stats_of(600);
		let mut app = App::new(&stats, "chat.jsonl", 60_000);
		assert_eq!(app.table.selected(), Some(0));
		press(&mut app, KeyCode::Up);
		assert_eq!(app.table.selected(), Some(0), "cannot scroll above the first");
		press(&mut app, KeyCode::PageDown);
		assert_eq!(app.table.selected(), Some(stats.authors.len() - 1), "clamped to the last");
	}

	#[test]
	fn q_quits_and_key_releases_do_nothing() {
		let stats = stats_of(60);
		let mut app = App::new(&stats, "chat.jsonl", 60_000);
		app.handle_key(KeyEvent::new_with_kind(KeyCode::Char('q'), ratatui::crossterm::event::KeyModifiers::NONE, KeyEventKind::Release));
		assert!(!app.quit);
		press(&mut app, KeyCode::Char('q'));
		assert!(app.quit);
	}
}
