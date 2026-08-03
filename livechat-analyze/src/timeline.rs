//! Placing a message in stream time.
//!
//! A replay says outright how far into the stream each message was said, so
//! there is nothing to work out. A live capture cannot know — it was watching a
//! page, not a timeline — and carries only YouTube's own send time, so the best
//! that can be done is to measure from the first message caught. That is a
//! different zero point, and a coarser clock, which is why the basis is reported
//! rather than quietly assumed.

use crate::record::Record;

/// Where the offsets came from, and so what they mean.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeBasis {
	/// `offset_ms`, straight from the replay: milliseconds into the stream.
	Replay,
	/// Derived from `sent_utc`: seconds since the first message we captured,
	/// which is not the same instant as the stream starting.
	SentUtc,
	/// Neither field survived, so there is no timeline to draw.
	None,
}

impl TimeBasis {
	/// How to describe the clock in the report, since the two bases do not mean
	/// the same thing and the difference changes how the histogram reads.
	pub fn describe(self) -> &'static str {
		match self {
			TimeBasis::Replay => "replay offsets",
			TimeBasis::SentUtc => "sent_utc, 1s resolution, from the first captured message",
			TimeBasis::None => "no timestamps",
		}
	}
}

/// Each record's position in stream time, in milliseconds, and where those
/// positions came from.
///
/// A single `offset_ms` anywhere in the file makes the whole file a replay: the
/// two bases measure from different zero points, so mixing them would put
/// messages in the wrong buckets rather than merely in imprecise ones.
pub fn offsets(records: &[Record]) -> (Vec<Option<i64>>, TimeBasis) {
	if records.iter().any(|r| r.offset_ms.is_some()) {
		return (records.iter().map(|r| r.offset_ms).collect(), TimeBasis::Replay);
	}

	let seconds: Vec<Option<i64>> = records.iter().map(|r| r.sent_utc.as_deref().and_then(parse_utc)).collect();
	let Some(first) = seconds.iter().flatten().min().copied() else {
		return (vec![None; records.len()], TimeBasis::None);
	};
	(seconds.iter().map(|s| s.map(|s| (s - first) * 1000)).collect(), TimeBasis::SentUtc)
}

/// `2026-07-26T22:15:30Z` to seconds since the epoch.
///
/// The archiver writes this one fixed shape and no other — `time::from_epoch_secs`
/// formats every one of them — so this reads the fields by position rather than
/// pulling in a date library to parse a format that cannot vary.
fn parse_utc(raw: &str) -> Option<i64> {
	let bytes = raw.as_bytes();
	if bytes.len() < 20 || bytes[10] != b'T' {
		return None;
	}
	let num = |range: std::ops::Range<usize>| -> Option<i64> { raw.get(range)?.parse().ok() };
	let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
	let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
	Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm), the same
/// one the archiver uses to write these timestamps in the first place.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
	let year = if month <= 2 {
		year - 1
	} else {
		year
	};
	let era = year.div_euclid(400);
	let yoe = year - era * 400;
	let mp = if month > 2 {
		month - 3
	} else {
		month + 9
	};
	let doy = (153 * mp + 2) / 5 + day - 1;
	let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
	era * 146_097 + doe - 719_468
}

/// Milliseconds into a stream as `1:23:45`. Chat starts before the stream does,
/// so the offset can be negative.
pub fn stream_time(ms: i64) -> String {
	let sign = if ms < 0 {
		"-"
	} else {
		""
	};
	let secs = ms.abs() / 1000;
	format!("{sign}{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// A span in milliseconds as `14h 12m`, or `2m 16s` when it is short enough that
/// minutes alone would round most of it away.
pub fn duration(ms: i64) -> String {
	let secs = ms.abs() / 1000;
	let (hours, minutes, seconds) = (secs / 3600, (secs % 3600) / 60, secs % 60);
	if hours > 0 {
		return format!("{hours}h {minutes:02}m");
	}
	if minutes > 0 {
		// A round number of minutes — which every bucket size off the ladder is —
		// reads worse with `00s` hung off the end of it.
		if seconds == 0 {
			return format!("{minutes}m");
		}
		return format!("{minutes}m {seconds:02}s");
	}
	format!("{seconds}s")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn records(lines: &[&str]) -> Vec<Record> {
		lines.iter().map(|l| Record::parse(l).unwrap().expect("a record")).collect()
	}

	#[test]
	fn a_replay_uses_its_own_offsets() {
		let records = records(&[
			r#"{"kind":"text","badges":[],"offset_ms":-4200,"sent_utc":"2026-03-14T00:11:29Z"}"#,
			r#"{"kind":"text","badges":[],"offset_ms":90000,"sent_utc":"2026-03-14T00:13:00Z"}"#,
		]);
		let (offsets, basis) = offsets(&records);
		assert_eq!(basis, TimeBasis::Replay);
		// Taken as they are, negatives included, rather than shifted to zero:
		// the replay is measuring from the stream start already.
		assert_eq!(offsets, vec![Some(-4200), Some(90000)]);
	}

	#[test]
	fn one_offset_settles_the_basis_for_the_whole_file() {
		// A file completed from its replay after being caught live has both, and
		// the two clocks have different zero points — so it is one or the other.
		let records = records(&[r#"{"kind":"text","badges":[],"sent_utc":"2026-03-14T00:11:29Z"}"#, r#"{"kind":"text","badges":[],"offset_ms":90000}"#]);
		let (offsets, basis) = offsets(&records);
		assert_eq!(basis, TimeBasis::Replay);
		assert_eq!(offsets, vec![None, Some(90000)]);
	}

	#[test]
	fn a_live_capture_measures_from_its_first_message() {
		let records = records(&[
			r#"{"kind":"text","badges":[],"sent_utc":"2026-07-26T22:15:30Z"}"#,
			r#"{"kind":"text","badges":[],"sent_utc":"2026-07-26T22:17:46Z"}"#,
			r#"{"kind":"text","badges":[],"sent_utc":"2026-07-26T22:15:30Z"}"#,
		]);
		let (offsets, basis) = offsets(&records);
		assert_eq!(basis, TimeBasis::SentUtc);
		assert_eq!(offsets, vec![Some(0), Some(136_000), Some(0)]);
	}

	#[test]
	fn the_zero_point_is_the_earliest_message_not_the_first_line() {
		// The archiver appends in arrival order, which a re-run over a replay can
		// leave out of send order.
		let records = records(&[
			r#"{"kind":"text","badges":[],"sent_utc":"2026-07-26T22:17:46Z"}"#,
			r#"{"kind":"text","badges":[],"sent_utc":"2026-07-26T22:15:30Z"}"#,
		]);
		let (offsets, _) = offsets(&records);
		assert_eq!(offsets, vec![Some(136_000), Some(0)]);
	}

	#[test]
	fn no_clock_at_all_leaves_nothing_to_place() {
		let records = records(&[r#"{"kind":"text","badges":[],"sent_utc":null}"#, r#"{"kind":"text","badges":[]}"#]);
		let (offsets, basis) = offsets(&records);
		assert_eq!(basis, TimeBasis::None);
		assert_eq!(offsets, vec![None, None]);
	}

	#[test]
	fn parses_the_one_timestamp_shape_the_archiver_writes() {
		assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
		assert_eq!(parse_utc("2026-07-26T22:15:30Z"), Some(1_785_104_130));
		// A leap day, since the date maths is the part that can be wrong.
		assert_eq!(parse_utc("2024-02-29T00:00:00Z"), Some(1_709_164_800));
		assert_eq!(parse_utc("yesterday"), None);
		assert_eq!(parse_utc("2026-07-26 22:15:30Z"), None);
	}

	#[test]
	fn renders_stream_times_and_spans() {
		assert_eq!(stream_time(0), "0:00:00");
		assert_eq!(stream_time(90_000), "0:01:30");
		assert_eq!(stream_time(-4200), "-0:00:04");
		assert_eq!(stream_time(51_120_000), "14:12:00");
		assert_eq!(duration(136_000), "2m 16s");
		assert_eq!(duration(6_830_000), "1h 53m");
		assert_eq!(duration(45_000), "45s");
		assert_eq!(duration(300_000), "5m");
	}
}
