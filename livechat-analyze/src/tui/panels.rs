//! The panels, and how the screen is divided between them.
//!
//! The arrangement follows a training monitor's: the chart takes most of the
//! room because it is the thing you actually read, the ranking sits beside it,
//! and the counts go underneath where they can be glanced at rather than
//! studied.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph, Row, Table, Wrap};

use crate::report::{commas, money};
use crate::stats::Stats;
use crate::timeline::{self, TimeBasis};
use crate::tui::app::App;

/// The chart's colour, and the accent everything else picks up so the dashboard
/// reads as one thing rather than a page of boxes.
const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
/// The focused chatter, kept well away from [`ACCENT`] so the one person and the
/// whole chat cannot be confused where the two lines run close together.
const FOCUS: Color = Color::Magenta;

pub fn draw(frame: &mut Frame, app: &mut App) {
	let area = frame.area();
	// The scrub bar is the only fixed-height row; everything above it shares
	// what is left, so a short terminal loses chart rows rather than panels.
	let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).split(area);
	let columns = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).split(rows[0]);
	let left = Layout::vertical([Constraint::Min(0), Constraint::Length(8)]).split(columns[0]);
	// Two rows for the keys: they no longer fit on one at any width this panel is
	// given, and a key list cut off mid-word is a key nobody finds.
	let right = Layout::vertical([Constraint::Min(0), Constraint::Length(4), Constraint::Length(7)]).split(columns[1]);

	activity(frame, app, left[0]);
	breakdown(frame, app.stats, left[1]);
	chatters(frame, app, right[0]);
	controls(frame, right[1]);
	status(frame, app, right[2]);
	scrub(frame, app, rows[1]);
}

/// Message volume over the stream: the panel everything else is context for.
fn activity(frame: &mut Frame, app: &App, area: Rect) {
	let focused = app.focused_author();
	// One chatter against a whole chat is a flat line at zero — a few hundred
	// messages is a rounding error next to twenty thousand — so their curve is
	// stretched to the chart's full height. That makes the height meaningless on
	// its own, which is why the peak it was stretched from is in the title, where
	// it is read whether or not the legend has room to draw itself.
	let focus_counts = focused.map(|author| app.buckets.overlay(&author.offsets));
	let focus_peak = focus_counts.iter().flatten().copied().max().unwrap_or(0);
	let title = match focused {
		Some(author) => format!(" Activity — {} buckets — {} peaks at {} ", timeline::duration(app.bucket_ms), author.name, commas(focus_peak)),
		None => format!(" Activity — {} buckets ", timeline::duration(app.bucket_ms)),
	};
	let block = Block::bordered().title(title).border_style(Style::default().fg(MUTED));

	if app.buckets.counts.is_empty() || !app.stats.has_timeline() {
		let note = match app.stats.basis {
			TimeBasis::None => "No message carries a timestamp, so there is no timeline to place them on.",
			_ => "Not enough of the chat is placed in time to draw a chart.",
		};
		frame.render_widget(
			Paragraph::new(note).style(Style::default().fg(MUTED)).block(block).wrap(Wrap {
				trim: true,
			}),
			area,
		);
		return;
	}

	// Plotted at the middle of each bucket: a bar's worth of messages happened
	// across it, not at the instant it opened.
	let half = app.buckets.size_ms as f64 / 2.0;
	let series: Vec<(f64, f64)> = app.buckets.counts.iter().enumerate().map(|(i, count)| (app.buckets.start_of(i) as f64 + half, *count as f64)).collect();

	let cursor_x = app.buckets.start_of(app.scrub) as f64 + half;
	let peak = app.buckets.peak().max(1) as f64;
	// A little headroom, so the tallest bar is not flush against the border.
	let top = peak * 1.15;
	let cursor = vec![(cursor_x, 0.0), (cursor_x, top)];

	let first = app.buckets.start_ms as f64;
	let last = app.buckets.start_of(app.buckets.counts.len().saturating_sub(1)) as f64 + app.buckets.size_ms as f64;

	let overlay: Vec<(f64, f64)> = match &focus_counts {
		Some(counts) => {
			let scale = peak / focus_peak.max(1) as f64;
			counts.iter().enumerate().map(|(i, count)| (app.buckets.start_of(i) as f64 + half, *count as f64 * scale)).collect()
		}
		None => Vec::new(),
	};

	let mut datasets = vec![
		// The cursor goes first so the volume is drawn over it rather than cut
		// in half by it.
		Dataset::default()
			.data(&cursor)
			.marker(symbols::Marker::Braille)
			.graph_type(GraphType::Line)
			.style(Style::default().fg(MUTED)),
		Dataset::default()
			.data(&series)
			.marker(symbols::Marker::Braille)
			.graph_type(GraphType::Area)
			.fill_to_y(0.0)
			// Dimmed while someone is in focus: a solid fill at full strength is
			// exactly the wrong backdrop for a thin line drawn across it.
			.style(match focused {
				Some(_) => Style::default().fg(ACCENT).add_modifier(Modifier::DIM),
				None => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
			}),
	];
	if let Some(author) = focused {
		datasets.push(
			// A line rather than another filled area: filled, it would bury the
			// chat it is meant to be read against.
			Dataset::default()
				.name(author.name.clone())
				.data(&overlay)
				.marker(symbols::Marker::Braille)
				.graph_type(GraphType::Line)
				.style(Style::default().fg(FOCUS).add_modifier(Modifier::BOLD)),
		);
	}

	let x_labels = vec![
		Span::raw(timeline::stream_time(app.buckets.start_ms)),
		Span::raw(timeline::stream_time(((first + last) / 2.0) as i64)),
		Span::raw(timeline::stream_time(last as i64)),
	];
	let y_labels = vec![Span::raw("0"), Span::raw(commas(app.buckets.peak()))];

	let chart = Chart::new(datasets)
		.block(block)
		.x_axis(Axis::default().style(Style::default().fg(MUTED)).bounds([first, last]).labels(x_labels))
		.y_axis(Axis::default().style(Style::default().fg(MUTED)).bounds([0.0, top]).labels(y_labels));
	frame.render_widget(chart, area);
}

/// Who talked, ranked, with a bar for the share so the shape of the ranking is
/// visible without reading the numbers.
fn chatters(frame: &mut Frame, app: &mut App, area: Rect) {
	let block = Block::bordered()
		.title(format!(" Top chatters — {} ", commas(app.stats.authors.len())))
		.border_style(Style::default().fg(MUTED));
	let total = app.stats.messages.max(1) as f64;

	// The bar is the first thing to give up room, down to nothing: a name cut to
	// eight characters identifies nobody and a squeezed percentage reads as a
	// different number, while the bar is only ever an impression.
	let bar_width = (area.width as usize).saturating_sub(36).min(16);
	let rows: Vec<Row> = app
		.stats
		.authors
		.iter()
		.enumerate()
		.map(|(rank, author)| {
			let share = author.count as f64 / total;
			Row::new(vec![
				Span::styled(format!("{:>3}", rank + 1), Style::default().fg(MUTED)),
				Span::raw(author.name.clone()),
				Span::styled(commas(author.count), Style::default().add_modifier(Modifier::BOLD)),
				Span::styled(format!("{:>5.1}%", share * 100.0), Style::default().fg(MUTED)),
				Span::styled(bar(share / peak_share(app.stats), bar_width), Style::default().fg(ACCENT)),
			])
		})
		.collect();

	let widths = [
		Constraint::Length(3),
		Constraint::Min(10),
		Constraint::Length(6),
		Constraint::Length(6),
		Constraint::Length(bar_width as u16),
	];
	// The selected row turns the overlay's colour while the overlay is up, which
	// is what ties the magenta line on the chart to a name over here.
	let highlight = match app.focus {
		true => Style::default().fg(FOCUS).add_modifier(Modifier::REVERSED),
		false => Style::default().add_modifier(Modifier::REVERSED),
	};
	let table = Table::new(rows, widths).block(block).row_highlight_style(highlight);
	frame.render_stateful_widget(table, area, &mut app.table);
}

/// The top chatter's share, which the bars are drawn against — scaling to 100%
/// would leave every bar a stub, since no one person is most of a chat.
fn peak_share(stats: &Stats) -> f64 {
	stats.authors.first().map_or(1.0, |a| (a.count as f64 / stats.messages.max(1) as f64).max(f64::EPSILON))
}

/// The counts that do not need a chart. Rows with nothing in them are left out
/// rather than printed empty — an ordinary chat has no superchats and no
/// badges, and a panel of blank headings tells you less than a shorter one.
fn breakdown(frame: &mut Frame, stats: &Stats, area: Rect) {
	let mut lines = Vec::new();

	// Money leads. A chat that took any is unusual, the panel is a fixed height,
	// and whatever falls off the bottom should be the ordinary counts rather
	// than the one thing that distinguishes this stream from every other.
	for (currency, stat) in &stats.money {
		let top = match &stat.top {
			Some((name, value)) => format!("   top {name} {currency}{}", money(*value)),
			None => String::new(),
		};
		lines.push(entry("Paid", format!("{currency} {} msgs   {currency}{} total{top}", commas(stat.messages), money(stat.total))));
	}
	if stats.unparsable_amounts > 0 {
		lines.push(entry("", format!("{} amounts were in a shape this could not read", commas(stats.unparsable_amounts))));
	}

	if stats.kinds.len() > 1 {
		lines.push(entry("Kinds", stats.kinds.iter().map(|(k, v)| format!("{k} {}", commas(*v))).collect::<Vec<_>>().join("   ")));
	}
	if !stats.badges.is_empty() {
		lines.push(entry("Badges", stats.badges.iter().map(|(k, v)| format!("{k} {}", commas(*v))).collect::<Vec<_>>().join("   ")));
	}
	if !stats.member_tiers.is_empty() {
		lines.push(entry("Tiers", stats.member_tiers.iter().map(|(k, v)| format!("{k} {}", commas(*v))).collect::<Vec<_>>().join("   ")));
	}
	if stats.lengths.max > 0 {
		lines.push(entry(
			"Messages",
			format!("mean {} chars   median {}   longest {}", stats.lengths.mean, stats.lengths.median, stats.lengths.max),
		));
	}
	if !stats.emoji.is_empty() {
		lines.push(entry("Emoji", stats.emoji.iter().take(8).map(|(k, v)| format!("{k} {}", commas(*v))).collect::<Vec<_>>().join("   ")));
	}

	let block = Block::bordered().title(" Breakdown ").border_style(Style::default().fg(MUTED));
	// The emoji row in particular runs long, and a row cut off mid-shortcode is
	// worse than one that takes a second line.
	frame.render_widget(
		Paragraph::new(lines).block(block).wrap(Wrap {
			trim: true,
		}),
		area,
	);
}

fn controls(frame: &mut Frame, area: Rect) {
	let key = Style::default().fg(ACCENT);
	let keys = Line::from(vec![
		Span::styled("←/→", key),
		Span::raw(" scrub "),
		Span::styled("+/-", key),
		Span::raw(" size "),
		Span::styled("↑/↓", key),
		Span::raw(" rank "),
		Span::styled("p", key),
		Span::raw(" peak "),
		Span::styled("f", key),
		Span::raw(" focus "),
		Span::styled("q", key),
		Span::raw(" quit"),
	]);
	let block = Block::bordered().title(" Controls ").border_style(Style::default().fg(MUTED));
	frame.render_widget(
		Paragraph::new(keys).block(block).wrap(Wrap {
			trim: true,
		}),
		area,
	);
}

fn status(frame: &mut Frame, app: &App, area: Rect) {
	let stats = app.stats;
	let mut lines = vec![
		Line::from(vec![
			Span::styled(commas(stats.messages), Style::default().add_modifier(Modifier::BOLD)),
			Span::raw(" messages   "),
			Span::styled(commas(stats.authors.len()), Style::default().add_modifier(Modifier::BOLD)),
			Span::raw(" authors"),
		]),
		Line::from(Span::raw(match stats.span_ms() {
			Some(span) => format!("span {}", timeline::duration(span)),
			None => "span unknown".to_string(),
		})),
		// The two bases measure from different zero points, so which one is in
		// play changes what the chart's left edge means.
		Line::from(Span::styled(stats.basis.describe(), Style::default().fg(MUTED))),
	];
	if stats.failures > 0 {
		lines.push(Line::from(Span::styled(
			format!("{} unparsable lines skipped", commas(stats.failures)),
			Style::default().fg(Color::Yellow),
		)));
	}
	lines.push(Line::from(Span::styled(app.title.to_string(), Style::default().fg(MUTED))));

	let block = Block::bordered().title(" Status ").border_style(Style::default().fg(MUTED));
	frame.render_widget(
		Paragraph::new(lines).block(block).wrap(Wrap {
			trim: true,
		}),
		area,
	);
}

/// Where the cursor is in the stream, and what is under it.
fn scrub(frame: &mut Frame, app: &App, area: Rect) {
	let (title, ratio) = if app.buckets.counts.is_empty() {
		(" no timeline ".to_string(), 0.0)
	} else {
		let count = app.buckets.counts[app.scrub];
		let from = timeline::stream_time(app.buckets.start_of(app.scrub));
		let to = timeline::stream_time(app.buckets.start_of(app.scrub) + app.buckets.size_ms);
		let last = app.buckets.counts.len().saturating_sub(1).max(1) as f64;
		// The overlay is drawn to its own scale, so the one number it cannot show
		// is how many of these messages were actually theirs. Here it is.
		let mine = match app.focused_author() {
			Some(author) => {
				let mine = app.buckets.overlay(&author.offsets)[app.scrub];
				format!("   {} from {} ", commas(mine), author.name)
			}
			None => " ".to_string(),
		};
		(format!(" {from} – {to}   {} messages{mine}", commas(count)), app.scrub as f64 / last)
	};

	// Drawn from two different characters rather than one in two colours, so the
	// cursor's position is still legible where colour is not — piped through
	// `--snapshot`, or on a terminal that has none.
	let width = area.width.saturating_sub(2) as usize;
	let filled = (ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
	let track = Line::from(vec![
		Span::styled("█".repeat(filled), Style::default().fg(ACCENT)),
		Span::styled("░".repeat(width.saturating_sub(filled)), Style::default().fg(MUTED)),
	]);

	let block = Block::bordered().title(title).border_style(Style::default().fg(MUTED));
	frame.render_widget(Paragraph::new(track).block(block), area);
}

fn entry(label: &str, value: String) -> Line<'static> {
	Line::from(vec![Span::styled(format!("{label:<9}"), Style::default().fg(MUTED)), Span::raw(value)])
}

/// A proportional bar. The eighth-block characters matter: without them every
/// share under one column rounds to nothing, and the long tail of a chat — which
/// is most of it — becomes a column of blanks.
fn bar(fraction: f64, width: usize) -> String {
	const EIGHTHS: [&str; 8] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];
	let fraction = fraction.clamp(0.0, 1.0);
	// Rounding alone sends the smallest shares to nothing, which is the case the
	// eighths exist for: anyone who spoke at all gets a visible mark.
	let eighths = match (fraction * width as f64 * 8.0).round() as usize {
		0 if fraction > 0.0 => 1,
		eighths => eighths,
	};
	let (full, rest) = (eighths / 8, eighths % 8);
	let mut out = "█".repeat(full.min(width));
	if rest > 0 && full < width {
		out.push_str(EIGHTHS[rest - 1]);
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn bars_scale_and_never_overflow() {
		assert_eq!(bar(1.0, 4), "████");
		assert_eq!(bar(0.0, 4), "");
		assert_eq!(bar(0.5, 4), "██");
		// Anything above zero shows something, which is the point of the eighths.
		assert!(!bar(0.01, 4).is_empty());
		assert!(bar(2.0, 4).chars().count() <= 4, "clamped rather than overrunning the column");
	}
}
