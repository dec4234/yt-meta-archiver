//! The plain-text report: what the dashboard says, in a form that survives a
//! pipe.
//!
//! This is not a lesser fallback. A terminal tool that can only draw itself on a
//! tty cannot be tested, redirected to a file, or read in CI, so the same
//! numbers are always available as flat text — no colour, no escape sequences,
//! no cursor movement.

use crate::stats::{Buckets, Stats};
use crate::timeline;

/// Writes the whole report.
pub fn render(stats: &Stats, title: &str, buckets: &Buckets, top: usize, emoji: usize, width: usize) -> String {
	let mut out = String::new();
	out.push_str(&header(stats, title));

	if !stats.authors.is_empty() {
		out.push_str(&chatters(stats, top));
	}
	if stats.has_timeline() && !buckets.counts.is_empty() {
		out.push_str(&activity(buckets, width));
	} else {
		out.push_str("\nActivity     no message carries a timestamp, so there is no timeline to place them on\n");
	}
	out.push_str(&breakdown(stats, emoji));
	out
}

fn header(stats: &Stats, title: &str) -> String {
	let mut out = format!("{title}\n  {} messages   {} authors", commas(stats.messages), commas(stats.authors.len()));
	if let Some(span) = stats.span_ms() {
		out.push_str(&format!("   {} span", timeline::duration(span)));
	}
	out.push_str(&format!("   ({})\n", stats.basis.describe()));
	if stats.failures > 0 {
		out.push_str(&format!("  {} lines could not be parsed and were skipped\n", commas(stats.failures)));
	}
	out
}

fn chatters(stats: &Stats, top: usize) -> String {
	let mut out = String::from("\nTop chatters\n");
	let widest = stats.authors.iter().take(top).map(|a| a.name.chars().count()).max().unwrap_or(0);
	for (rank, author) in stats.authors.iter().take(top).enumerate() {
		let share = author.count as f64 / stats.messages.max(1) as f64 * 100.0;
		let count = commas(author.count);
		out.push_str(&format!("  {:>3}  {:<widest$}  {:>7}  {share:>5.1}%\n", rank + 1, author.name, count));
	}
	if stats.authors.len() > top {
		out.push_str(&format!("       … and {} more\n", commas(stats.authors.len() - top)));
	}
	out
}

fn activity(buckets: &Buckets, width: usize) -> String {
	let peak = buckets.peak().max(1);
	let peak_at = buckets.peak_index();
	let mut out = format!(
		"\nActivity ({} buckets, peak {} with {})\n",
		timeline::duration(buckets.size_ms),
		buckets.label(peak_at),
		commas(buckets.counts[peak_at])
	);
	let label_width = (0..buckets.counts.len()).map(|i| buckets.label(i).chars().count()).max().unwrap_or(0);
	for (index, count) in buckets.counts.iter().enumerate() {
		// Scaled against the peak, so the tallest bar is always the full width
		// and the shape is readable whatever the absolute numbers are.
		let bar = count * width / peak;
		out.push_str(&format!("  {:>label_width$}  {:<width$}  {:>7}\n", buckets.label(index), "#".repeat(bar), commas(*count)));
	}
	out
}

fn breakdown(stats: &Stats, emoji: usize) -> String {
	let mut out = String::new();

	// Money leads, as it does on the dashboard: a chat that took any is the
	// unusual case, and it should not be read last.
	for (currency, stat) in &stats.money {
		let top = match &stat.top {
			Some((name, value)) => format!("   top {name} {currency}{}", money(*value)),
			None => String::new(),
		};
		out.push_str(&format!("\nPaid         {currency}  {} msgs   {currency}{} total{top}\n", commas(stat.messages), money(stat.total)));
	}
	if stats.unparsable_amounts > 0 {
		out.push_str(&format!("             {} amounts were in a shape this could not read\n", commas(stats.unparsable_amounts)));
	}

	// A transcript of nothing but ordinary messages says nothing by listing the
	// one kind it has, and both files in this repo are exactly that.
	if stats.kinds.len() > 1 {
		out.push_str(&format!("\nKinds        {}\n", pairs(stats.kinds.iter().map(|(k, v)| (k.clone(), *v)))));
	}
	if !stats.badges.is_empty() {
		out.push_str(&format!("Badges       {}\n", pairs(stats.badges.iter().map(|(k, v)| (k.clone(), *v)))));
		if !stats.member_tiers.is_empty() {
			out.push_str(&format!("Tiers        {}\n", pairs(stats.member_tiers.iter().map(|(k, v)| (k.clone(), *v)))));
		}
	}
	if stats.lengths.max > 0 {
		out.push_str(&format!(
			"Messages     mean {} chars   median {}   longest {}\n",
			stats.lengths.mean, stats.lengths.median, stats.lengths.max
		));
	}
	if !stats.shortcodes.is_empty() {
		out.push_str(&format!("Emoji        {}\n", pairs(stats.shortcodes.iter().take(emoji).map(|(k, v)| (format!(":{k}:"), *v)))));
	}
	out
}

/// `k v   k v   k v`, the shape every one-line section uses.
fn pairs(entries: impl Iterator<Item = (String, usize)>) -> String {
	entries.map(|(k, v)| format!("{k} {}", commas(v))).collect::<Vec<_>>().join("   ")
}

/// `23700` -> `23,700`. Chat counts run to five figures and are unreadable
/// without it.
pub fn commas(n: usize) -> String {
	let digits = n.to_string();
	let mut out = String::with_capacity(digits.len() + digits.len() / 3);
	for (i, c) in digits.chars().enumerate() {
		if i > 0 && (digits.len() - i).is_multiple_of(3) {
			out.push(',');
		}
		out.push(c);
	}
	out
}

/// `1234.56` -> `1,234.56`. Grouped like the counts are, since a superchat total
/// running to four figures is exactly the one worth reading at a glance.
pub fn money(value: f64) -> String {
	let whole = value.abs().trunc();
	let pence = ((value.abs() - whole) * 100.0).round() as usize;
	let sign = if value < 0.0 {
		"-"
	} else {
		""
	};
	format!("{sign}{}.{pence:02}", commas(whole as usize))
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

	#[test]
	fn groups_digits() {
		assert_eq!(commas(0), "0");
		assert_eq!(commas(999), "999");
		assert_eq!(commas(1_000), "1,000");
		assert_eq!(commas(23_700), "23,700");
		assert_eq!(commas(1_234_567), "1,234,567");
	}

	#[test]
	fn groups_money_and_keeps_both_decimals() {
		assert_eq!(money(5.0), "5.00");
		assert_eq!(money(1234.56), "1,234.56");
		assert_eq!(money(0.5), "0.50");
		// A round total must not lose its pence to truncation.
		assert_eq!(money(1_000_000.0), "1,000,000.00");
	}

	#[test]
	fn a_plain_transcript_prints_no_empty_headings() {
		let stats = stats_from(&[
			r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"hello","sent_utc":"2026-07-26T22:15:30Z"}"#,
			r#"{"kind":"text","author":"@b","author_channel_id":"UCb","badges":[],"text":"hi","sent_utc":"2026-07-26T22:15:40Z"}"#,
		]);
		let buckets = stats::buckets(&stats.offsets, 10_000);
		let out = render(&stats, "chat.jsonl", &buckets, 20, 10, 40);

		assert!(out.contains("2 messages   2 authors"));
		assert!(out.contains("Top chatters"));
		assert!(out.contains("Activity"));
		// Nothing to say about kinds, badges, emoji or money, so nothing is said.
		assert!(!out.contains("Kinds"));
		assert!(!out.contains("Badges"));
		assert!(!out.contains("Emoji"));
		assert!(!out.contains("Paid"));
	}

	#[test]
	fn a_richer_transcript_prints_the_sections_it_has() {
		let stats = stats_from(&[
			r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":":kiss:","offset_ms":0}"#,
			r#"{"kind":"superchat","author":"@b","author_channel_id":"UCb","badges":["Member (2 years)"],"text":"hi","amount":"£5.00","offset_ms":60000}"#,
		]);
		let buckets = stats::buckets(&stats.offsets, 30_000);
		let out = render(&stats, "chat.jsonl", &buckets, 20, 10, 40);

		assert!(out.contains("replay offsets"));
		assert!(out.contains("Kinds"));
		assert!(out.contains("superchat 1"));
		assert!(out.contains("Badges       Member 1"));
		assert!(out.contains("Member (2 years)"));
		assert!(out.contains(":kiss: 1"));
		assert!(out.contains("Paid         £  1 msgs   £5.00 total"));
		assert!(out.contains("top @b £5.00"));
	}

	#[test]
	fn says_so_when_there_is_no_timeline() {
		let stats = stats_from(&[r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"hello"}"#]);
		let buckets = stats::buckets(&stats.offsets, 10_000);
		let out = render(&stats, "chat.jsonl", &buckets, 20, 10, 40);
		assert!(out.contains("no timeline"));
		// Everything that does not need a clock is still reported.
		assert!(out.contains("Top chatters"));
	}

	#[test]
	fn reports_unparsable_lines_and_amounts() {
		let records = vec![
			Record::parse(r#"{"kind":"superchat","author":"@a","author_channel_id":"UCa","badges":[],"amount":"free","offset_ms":0}"#)
				.unwrap()
				.expect("a record"),
		];
		let stats = stats::collect(&records, 3);
		let buckets = stats::buckets(&stats.offsets, 10_000);
		let out = render(&stats, "chat.jsonl", &buckets, 20, 10, 40);
		assert!(out.contains("3 lines could not be parsed"));
		assert!(out.contains("1 amounts were in a shape this could not read"));
	}
}
