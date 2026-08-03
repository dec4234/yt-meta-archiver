//! The aggregation pass: one walk over the records, everything counted at once.
//!
//! Bucketing is deliberately *not* part of the result. The dashboard lets the
//! bucket size be changed with a keypress, and re-reading a 6 MB transcript for
//! that would be absurd — so [`buckets`] is a function of the offsets already in
//! memory, and gets called again on every change.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::record::Record;
use crate::timeline::{self, TimeBasis};

/// The bucket sizes `auto` chooses between and `+`/`-` step through, in
/// milliseconds. Anything else lands on labels nobody reads at a glance —
/// `0:07:30` buckets are arithmetically fine and useless to look at.
pub const LADDER: [i64; 7] = [10_000, 30_000, 60_000, 300_000, 600_000, 1_800_000, 3_600_000];

/// What `auto` aims for. Enough bars to show the shape of a stream, few enough
/// that each one is still worth a column.
const TARGET_BUCKETS: i64 = 40;

/// One chatter.
pub struct AuthorStat {
	/// What they were grouped by — see [`Record::author_key`]. Kept so `--author`
	/// can be given a channel id, which is the only handle-proof way to name
	/// someone.
	pub key: String,
	pub name: String,
	pub count: usize,
	pub badges: BTreeSet<String>,
	pub money: BTreeMap<String, f64>,
	/// This chatter's messages in stream time, in the same shape as
	/// [`Stats::offsets`] so they can be bucketed by the same code. Kept per
	/// author rather than recomputed by filtering, because the dashboard changes
	/// which author is in focus on a keypress.
	pub offsets: Vec<Option<i64>>,
}

impl AuthorStat {
	/// When they first and last spoke, in stream time.
	pub fn range(&self) -> Option<(i64, i64)> {
		let mut placed = self.offsets.iter().flatten().copied();
		let first = placed.next()?;
		Some(placed.fold((first, first), |(lo, hi), at| (lo.min(at), hi.max(at))))
	}
}

/// One currency's takings. No conversion between them — an exchange rate is a
/// thing with a date, and inventing one would make the total a guess.
pub struct MoneyStat {
	pub messages: usize,
	pub total: f64,
	/// The largest single payment, and who made it.
	pub top: Option<(String, f64)>,
}

/// Message lengths, in characters.
pub struct Lengths {
	pub mean: usize,
	pub median: usize,
	pub max: usize,
}

pub struct Stats {
	pub messages: usize,
	pub failures: usize,
	/// Chatters, most first.
	pub authors: Vec<AuthorStat>,
	pub kinds: BTreeMap<String, usize>,
	/// Messages per badge, with the membership tiers collapsed into one
	/// `Member` entry — see [`member_tiers`](Stats::member_tiers).
	pub badges: BTreeMap<String, usize>,
	/// The tier wording seen, kept separately because it varies
	/// (`Member (2 years)`, `Member (6 months)`) and would otherwise splinter
	/// the badge counts into a row per tier.
	pub member_tiers: BTreeMap<String, usize>,
	/// Emoji shortcodes, most first.
	pub shortcodes: Vec<(String, usize)>,
	pub lengths: Lengths,
	pub money: BTreeMap<String, MoneyStat>,
	/// Amounts in a shape [`parse_amount`] did not recognise. Shown rather than
	/// dropped, so a currency we mis-handle is visible instead of silent.
	pub unparsable_amounts: usize,
	/// Each message's place in stream time, kept so the buckets can be redrawn.
	pub offsets: Vec<Option<i64>>,
	pub basis: TimeBasis,
	pub first: Option<i64>,
	pub last: Option<i64>,
}

impl Stats {
	/// The span the chat covers, in milliseconds.
	pub fn span_ms(&self) -> Option<i64> {
		Some(self.last? - self.first?)
	}

	/// Whether there is a timeline to draw at all.
	pub fn has_timeline(&self) -> bool {
		self.basis != TimeBasis::None && self.span_ms().is_some()
	}
}

/// Everything, in one pass.
pub fn collect(records: &[Record], failures: usize) -> Stats {
	let (offsets, basis) = timeline::offsets(records);

	let mut authors: HashMap<String, AuthorStat> = HashMap::new();
	let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
	let mut badges: BTreeMap<String, usize> = BTreeMap::new();
	let mut member_tiers: BTreeMap<String, usize> = BTreeMap::new();
	let mut shortcodes: HashMap<String, usize> = HashMap::new();
	let mut money: BTreeMap<String, MoneyStat> = BTreeMap::new();
	let mut unparsable_amounts = 0;
	let mut lengths = Vec::new();

	for (record, offset) in records.iter().zip(&offsets) {
		*kinds.entry(record.kind.clone()).or_default() += 1;

		let key = record.author_key();
		let author = authors.entry(key.clone()).or_insert_with(|| AuthorStat {
			key,
			name: record.display_name(),
			count: 0,
			badges: BTreeSet::new(),
			money: BTreeMap::new(),
			offsets: Vec::new(),
		});
		author.count += 1;
		author.offsets.push(*offset);

		for badge in &record.badges {
			author.badges.insert(badge.clone());
			*badges.entry(collapse_badge(badge).to_string()).or_default() += 1;
			if badge.starts_with("Member") {
				*member_tiers.entry(badge.clone()).or_default() += 1;
			}
		}

		if let Some(text) = &record.text {
			lengths.push(text.chars().count());
			for code in scan_shortcodes(text) {
				*shortcodes.entry(code).or_default() += 1;
			}
		}

		if let Some(raw) = &record.amount {
			match parse_amount(raw) {
				Some((currency, value)) => {
					*author.money.entry(currency.clone()).or_default() += value;
					let stat = money.entry(currency).or_insert_with(|| MoneyStat {
						messages: 0,
						total: 0.0,
						top: None,
					});
					stat.messages += 1;
					stat.total += value;
					if stat.top.as_ref().is_none_or(|(_, best)| value > *best) {
						stat.top = Some((record.display_name(), value));
					}
				}
				None => unparsable_amounts += 1,
			}
		}
	}

	let mut authors: Vec<AuthorStat> = authors.into_values().collect();
	// Ties broken by name so the ranking is stable between runs; a HashMap's
	// order is not, and a table that reshuffles itself is hard to trust.
	authors.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));

	let mut shortcodes: Vec<(String, usize)> = shortcodes.into_iter().collect();
	shortcodes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

	let placed: Vec<i64> = offsets.iter().flatten().copied().collect();

	Stats {
		messages: records.len(),
		failures,
		authors,
		kinds,
		badges,
		member_tiers,
		shortcodes,
		lengths: summarise_lengths(lengths),
		money,
		unparsable_amounts,
		first: placed.iter().min().copied(),
		last: placed.iter().max().copied(),
		offsets,
		basis,
	}
}

/// Messages per time bucket.
pub struct Buckets {
	/// Where the first bucket begins — not necessarily zero, since replay chat
	/// starts before the stream does.
	pub start_ms: i64,
	pub size_ms: i64,
	pub counts: Vec<usize>,
}

impl Buckets {
	pub fn peak(&self) -> usize {
		self.counts.iter().copied().max().unwrap_or(0)
	}

	pub fn peak_index(&self) -> usize {
		self.counts.iter().enumerate().max_by_key(|(_, c)| **c).map_or(0, |(i, _)| i)
	}

	/// When bucket `index` begins, in stream time.
	pub fn start_of(&self, index: usize) -> i64 {
		self.start_ms + index as i64 * self.size_ms
	}

	/// A subset of the messages counted on *this* grid rather than on one of
	/// their own.
	///
	/// One chatter's messages bucketed independently would start at their first
	/// message and end at their last, which is a different set of bars from the
	/// stream's — and comparing the two, which is the whole point of looking at
	/// one person, would mean comparing bar 3 of one chart with bar 11 of the
	/// other.
	pub fn overlay(&self, offsets: &[Option<i64>]) -> Vec<usize> {
		let mut counts = vec![0; self.counts.len()];
		if counts.is_empty() {
			return counts;
		}
		for offset in offsets.iter().flatten() {
			let index = (offset - self.start_ms).max(0) / self.size_ms;
			counts[(index as usize).min(self.counts.len() - 1)] += 1;
		}
		counts
	}

	/// A bucket's label. Whole seconds are noise on anything coarser than a
	/// minute, and the only thing that distinguishes adjacent buckets below one.
	pub fn label(&self, index: usize) -> String {
		let time = timeline::stream_time(self.start_of(index));
		if self.size_ms < 60_000 {
			return time;
		}
		// `0:05:00` -> `0:05`, dropping the seconds that are always zero.
		time.rsplit_once(':').map_or(time.clone(), |(head, _)| head.to_string())
	}
}

/// The messages of each bucket, at the given size.
///
/// The first bucket is aligned to the first message rather than to a round
/// multiple of the size: with negative offsets in play, aligning to zero would
/// put pre-stream chat in a bucket labelled after the stream had started.
pub fn buckets(offsets: &[Option<i64>], size_ms: i64) -> Buckets {
	let size_ms = size_ms.max(1);
	let placed: Vec<i64> = offsets.iter().flatten().copied().collect();
	let (Some(first), Some(last)) = (placed.iter().min().copied(), placed.iter().max().copied()) else {
		return Buckets {
			start_ms: 0,
			size_ms,
			counts: Vec::new(),
		};
	};

	// Inclusive of the last message's own bucket, so the final bar is not lost
	// to integer division.
	let count = ((last - first) / size_ms + 1) as usize;
	let mut counts = vec![0; count];
	for offset in placed {
		let index = ((offset - first) / size_ms) as usize;
		counts[index.min(count - 1)] += 1;
	}
	Buckets {
		start_ms: first,
		size_ms,
		counts,
	}
}

/// Which chatter a `--author` argument names, as an index into
/// [`Stats::authors`].
///
/// An exact handle or channel id wins outright, so `@bot` cannot be stolen by
/// `@bottom` merely because that one is louder. Failing that a substring is
/// enough, since handles are long and awkward to type — but only while it names
/// one person, because picking the busiest of several matches would silently
/// answer a different question than the one asked.
pub fn find_author(authors: &[AuthorStat], query: &str) -> Result<usize, String> {
	let query = query.trim();
	let matches = |field: &str| field.eq_ignore_ascii_case(query);
	if let Some(index) = authors.iter().position(|a| matches(&a.name) || matches(&a.key)) {
		return Ok(index);
	}

	let needle = query.to_lowercase();
	let mut found = authors
		.iter()
		.enumerate()
		.filter(|(_, a)| a.name.to_lowercase().contains(&needle) || a.key.to_lowercase().contains(&needle));
	let Some((index, _)) = found.next() else {
		return Err(format!("no chatter here is called {query:?}"));
	};
	let ambiguous: Vec<&str> = std::iter::once(authors[index].name.as_str()).chain(found.map(|(_, a)| a.name.as_str())).take(6).collect();
	if ambiguous.len() > 1 {
		return Err(format!("{query:?} matches {} — try one of them", ambiguous.join(", ")));
	}
	Ok(index)
}

/// A bucket size for a span, off the ladder, aiming for [`TARGET_BUCKETS`].
pub fn auto_bucket(span_ms: i64) -> i64 {
	let ideal = (span_ms / TARGET_BUCKETS).max(1);
	// The first rung at or above the ideal, so the count errs towards fewer,
	// wider bars rather than more than will fit on a screen.
	LADDER.iter().copied().find(|&step| step >= ideal).unwrap_or(*LADDER.last().expect("the ladder is not empty"))
}

/// The rung after this one, in either direction, for the `+`/`-` keys.
pub fn step_bucket(current: i64, coarser: bool) -> i64 {
	if coarser {
		return LADDER.iter().copied().find(|&step| step > current).unwrap_or(current);
	}
	LADDER.iter().copied().rev().find(|&step| step < current).unwrap_or(current)
}

/// `30s`, `5m`, `1h` — or `auto`, which needs the span and so is resolved later.
pub fn parse_bucket_spec(spec: &str) -> Option<Option<i64>> {
	let spec = spec.trim();
	if spec.eq_ignore_ascii_case("auto") {
		return Some(None);
	}
	let (number, unit) = spec.split_at(spec.find(|c: char| !c.is_ascii_digit())?);
	let number: i64 = number.parse().ok()?;
	if number == 0 {
		return None;
	}
	let scale = match unit {
		"s" => 1_000,
		"m" => 60_000,
		"h" => 3_600_000,
		_ => return None,
	};
	Some(Some(number * scale))
}

/// The `:shortcode:` tokens in a message.
///
/// Both capture paths flatten emoji runs to the shortcode a viewer would have
/// typed, so this is the whole of the emoji data — there are no raw characters
/// to count. Scanning colon to colon rather than splitting on them is what makes
/// `:kiss::kiss:` two emoji instead of one: a split sees an empty field between
/// the pair and loses track of which colons open and which close.
pub fn scan_shortcodes(text: &str) -> Vec<String> {
	let mut out = Vec::new();
	let bytes = text.as_bytes();
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] != b':' {
			i += 1;
			continue;
		}
		// The body has to look like a shortcode, which is what keeps `https://`
		// and ordinary prose punctuation out of the counts.
		let mut j = i + 1;
		while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'_' | b'+' | b'-')) {
			j += 1;
		}
		if j > i + 1 && j < bytes.len() && bytes[j] == b':' {
			out.push(text[i + 1..j].to_string());
			// Past the closing colon, so it cannot also open the next token.
			i = j + 1;
		} else {
			i += 1;
		}
	}
	out
}

/// `£5.00` -> `("£", 5.0)`. The currency is whatever leads the number, which is
/// how YouTube writes it: a symbol, a symbol with a country in front of it
/// (`CA$`), or a bare code (`SGD 5.00`).
pub fn parse_amount(raw: &str) -> Option<(String, f64)> {
	let raw = raw.trim();
	let split = raw.find(|c: char| c.is_ascii_digit())?;
	let currency = raw[..split].trim();
	// Group separators vary by locale but never carry meaning here.
	let value: f64 = raw[split..].replace(',', "").parse().ok()?;
	let currency = if currency.is_empty() {
		"?"
	} else {
		currency
	};
	Some((currency.to_string(), value))
}

/// `Member (2 years)` and `Member (6 months)` are the same badge worn for
/// different lengths of time, and counting them apart buries how many members
/// were in the chat under a row each.
fn collapse_badge(badge: &str) -> &str {
	if badge.starts_with("Member") {
		return "Member";
	}
	badge
}

fn summarise_lengths(mut lengths: Vec<usize>) -> Lengths {
	if lengths.is_empty() {
		return Lengths {
			mean: 0,
			median: 0,
			max: 0,
		};
	}
	lengths.sort_unstable();
	Lengths {
		mean: lengths.iter().sum::<usize>() / lengths.len(),
		median: lengths[lengths.len() / 2],
		max: *lengths.last().expect("lengths is not empty"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_the_currencies_youtube_writes() {
		assert_eq!(parse_amount("£5.00"), Some(("£".to_string(), 5.0)));
		assert_eq!(parse_amount("$1,234.56"), Some(("$".to_string(), 1234.56)));
		assert_eq!(parse_amount("CA$10.00"), Some(("CA$".to_string(), 10.0)));
		assert_eq!(parse_amount("¥500"), Some(("¥".to_string(), 500.0)));
		assert_eq!(parse_amount("SGD 5.00"), Some(("SGD".to_string(), 5.0)));
	}

	#[test]
	fn refuses_amounts_it_cannot_read() {
		assert_eq!(parse_amount("free"), None);
		assert_eq!(parse_amount(""), None);
		// Counted as unparsable rather than guessed at.
		assert_eq!(parse_amount("£"), None);
	}

	#[test]
	fn counts_adjacent_shortcodes_separately() {
		assert_eq!(scan_shortcodes(":kiss::kiss:"), vec!["kiss", "kiss"]);
		assert_eq!(scan_shortcodes("uff:collision:"), vec!["collision"]);
		// Custom channel emoji use hyphens, which real transcripts are full of.
		assert_eq!(scan_shortcodes(":face-green-smiling: hi :hand-pink-waving:"), vec!["face-green-smiling", "hand-pink-waving"]);
	}

	#[test]
	fn leaves_colons_that_are_not_emoji_alone() {
		assert!(scan_shortcodes("see https://youtu.be/x").is_empty());
		assert!(scan_shortcodes("time: 22:15").is_empty());
		assert!(scan_shortcodes("::").is_empty());
		assert!(scan_shortcodes("no colons here").is_empty());
	}

	fn records(lines: &[&str]) -> Vec<Record> {
		lines.iter().map(|l| Record::parse(l).unwrap().expect("a record")).collect()
	}

	#[test]
	fn counts_a_transcript() {
		let records = records(&[
			r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"hello","sent_utc":"2026-07-26T22:15:30Z"}"#,
			r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":":kiss:","sent_utc":"2026-07-26T22:15:40Z"}"#,
			r#"{"kind":"superchat","author":"@b","author_channel_id":"UCb","badges":["Member (2 years)"],"text":"hi","amount":"£5.00","sent_utc":"2026-07-26T22:16:00Z"}"#,
		]);
		let stats = collect(&records, 1);
		assert_eq!(stats.messages, 3);
		assert_eq!(stats.failures, 1);
		assert_eq!(stats.authors.len(), 2);
		assert_eq!(stats.authors[0].name, "@a");
		assert_eq!(stats.authors[0].count, 2);
		assert_eq!(stats.kinds["text"], 2);
		assert_eq!(stats.kinds["superchat"], 1);
		// The tier is collapsed for the count and kept for the detail.
		assert_eq!(stats.badges["Member"], 1);
		assert_eq!(stats.member_tiers["Member (2 years)"], 1);
		assert_eq!(stats.shortcodes, vec![("kiss".to_string(), 1)]);
		assert_eq!(stats.money["£"].messages, 1);
		assert_eq!(stats.money["£"].total, 5.0);
		assert_eq!(stats.money["£"].top, Some(("@b".to_string(), 5.0)));
		assert_eq!(stats.span_ms(), Some(30_000));
	}

	#[test]
	fn keeps_each_chatters_own_messages_in_stream_time() {
		let records = records(&[
			r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"x","offset_ms":0}"#,
			r#"{"kind":"text","author":"@b","author_channel_id":"UCb","badges":[],"text":"x","offset_ms":30000}"#,
			r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"x","offset_ms":90000}"#,
		]);
		let stats = collect(&records, 0);
		let a = &stats.authors[0];
		assert_eq!(a.name, "@a");
		assert_eq!(a.key, "UCa");
		assert_eq!(a.offsets, vec![Some(0), Some(90_000)]);
		assert_eq!(a.range(), Some((0, 90_000)));

		// And they land on the stream's own grid, not one of their own: @a's
		// second message is in bucket 3, where the chart draws it.
		let buckets = buckets(&stats.offsets, 30_000);
		assert_eq!(buckets.overlay(&a.offsets), vec![1, 0, 0, 1]);
		assert_eq!(buckets.overlay(&stats.authors[1].offsets), vec![0, 1, 0, 0]);
	}

	#[test]
	fn an_overlay_totals_the_chatters_own_messages_and_no_more() {
		let stats = collect(
			&records(&[
				r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"x","offset_ms":0}"#,
				r#"{"kind":"text","author":"@a","author_channel_id":"UCa","badges":[],"text":"x"}"#,
			]),
			0,
		);
		let buckets = buckets(&stats.offsets, 30_000);
		// The unplaced message is counted for them but has nowhere to be drawn,
		// which is the same treatment the chart gives it.
		assert_eq!(stats.authors[0].count, 2);
		assert_eq!(buckets.overlay(&stats.authors[0].offsets).iter().sum::<usize>(), 1);
	}

	#[test]
	fn names_a_chatter_by_handle_id_or_enough_of_either() {
		let stats = collect(
			&records(&[
				r#"{"kind":"text","author":"@bottom","author_channel_id":"UCbottom","badges":[],"text":"x","offset_ms":0}"#,
				r#"{"kind":"text","author":"@bottom","author_channel_id":"UCbottom","badges":[],"text":"x","offset_ms":1000}"#,
				r#"{"kind":"text","author":"@bot","author_channel_id":"UCbot","badges":[],"text":"x","offset_ms":2000}"#,
			]),
			0,
		);
		let name = |i: usize| stats.authors[i].name.as_str();

		assert_eq!(find_author(&stats.authors, "@bottom").map(name), Ok("@bottom"));
		assert_eq!(find_author(&stats.authors, "UCbot").map(name), Ok("@bot"));
		// An exact handle is not stolen by the busier chatter it is a prefix of.
		assert_eq!(find_author(&stats.authors, "@BOT").map(name), Ok("@bot"));
		// A substring that names one person is enough; one that names two is not.
		assert_eq!(find_author(&stats.authors, "ttom").map(name), Ok("@bottom"));
		assert!(find_author(&stats.authors, "bot").is_err());
		assert!(find_author(&stats.authors, "@nobody").is_err());
	}

	#[test]
	fn buckets_hold_every_message_at_every_size() {
		let offsets: Vec<Option<i64>> = (0..500).map(|i| Some(i * 1000)).collect();
		for size in LADDER {
			let buckets = buckets(&offsets, size);
			assert_eq!(buckets.counts.iter().sum::<usize>(), 500, "at {size}ms");
		}
	}

	#[test]
	fn a_message_lands_in_the_bucket_its_offset_names() {
		// Boundaries belong to the bucket they open, not the one they close.
		let offsets = vec![Some(0), Some(9_999), Some(10_000), Some(19_999), Some(20_000)];
		let buckets = buckets(&offsets, 10_000);
		assert_eq!(buckets.counts, vec![2, 2, 1]);
		assert_eq!(buckets.start_ms, 0);
		assert_eq!(buckets.peak(), 2);
	}

	#[test]
	fn pre_stream_chat_keeps_its_negative_labels() {
		let offsets = vec![Some(-90_000), Some(-30_000), Some(30_000)];
		let buckets = buckets(&offsets, 60_000);
		assert_eq!(buckets.start_ms, -90_000);
		assert_eq!(buckets.counts, vec![1, 1, 1]);
		assert!(buckets.label(0).starts_with('-'));
	}

	#[test]
	fn no_offsets_means_no_buckets_rather_than_a_panic() {
		let buckets = buckets(&[None, None], 60_000);
		assert!(buckets.counts.is_empty());
		assert_eq!(buckets.peak(), 0);
	}

	#[test]
	fn auto_picks_a_readable_step_off_the_ladder() {
		// The 1h53m file in the repo: 40 buckets would be ~2m50s, so 5m.
		assert_eq!(auto_bucket(6_830_000), 300_000);
		// A two-minute capture wants the finest rung, not a single bar.
		assert_eq!(auto_bucket(136_000), 10_000);
		// A fourteen-hour stream tops out rather than running off the end.
		assert_eq!(auto_bucket(51_120_000), 1_800_000);
		assert!(LADDER.contains(&auto_bucket(999_999_999)));
	}

	#[test]
	fn stepping_stops_at_the_ends_of_the_ladder() {
		assert_eq!(step_bucket(60_000, true), 300_000);
		assert_eq!(step_bucket(60_000, false), 30_000);
		assert_eq!(step_bucket(10_000, false), 10_000);
		assert_eq!(step_bucket(3_600_000, true), 3_600_000);
	}

	#[test]
	fn reads_bucket_specs() {
		assert_eq!(parse_bucket_spec("30s"), Some(Some(30_000)));
		assert_eq!(parse_bucket_spec("5m"), Some(Some(300_000)));
		assert_eq!(parse_bucket_spec("1h"), Some(Some(3_600_000)));
		assert_eq!(parse_bucket_spec("auto"), Some(None));
		assert_eq!(parse_bucket_spec("AUTO"), Some(None));
		assert_eq!(parse_bucket_spec("5"), None);
		assert_eq!(parse_bucket_spec("0s"), None);
		assert_eq!(parse_bucket_spec("5x"), None);
	}
}
