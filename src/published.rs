//! Exact publication dates for community posts.
//!
//! The posts feed only ever renders a relative time ("2 years ago"), but each
//! post's own page carries a schema.org `DiscussionForumPosting` block with the
//! real timestamp, so the date comes from there.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// The JSON-LD block sits near the end of a ~750 KB page, so the whole thing
/// has to be read; this is only a guard against a runaway response.
const MAX_PAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Fetches a post's page and returns when it was published, as an ISO 8601 UTC
/// timestamp.
pub fn fetch_utc(post_id: &str) -> Result<String> {
    let url = format!("https://www.youtube.com/post/{post_id}");
    let html = ureq::get(&url)
        .header("user-agent", USER_AGENT)
        .header("accept-language", "en-US,en")
        .call()?
        .body_mut()
        .with_config()
        .limit(MAX_PAGE_BYTES)
        .read_to_string()?;

    let raw = extract_date_published(&html)
        .ok_or_else(|| anyhow!("post page carried no datePublished"))?;
    to_utc(raw)
}

/// Now, as an ISO 8601 UTC timestamp.
pub fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_utc(secs as i64)
}

fn extract_date_published(html: &str) -> Option<&str> {
    const KEY: &str = "\"datePublished\":\"";
    let rest = &html[html.find(KEY)? + KEY.len()..];
    Some(&rest[..rest.find('"')?])
}

/// `2023-10-03T10:20:08.354091-07:00` -> `2023-10-03T17:20:08Z`. Fractional
/// seconds are dropped; the offset is applied.
fn to_utc(raw: &str) -> Result<String> {
    let bytes = raw.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' {
        bail!("unrecognised timestamp {raw:?}");
    }
    let num = |range: std::ops::Range<usize>| -> Result<i64> {
        raw[range.clone()]
            .parse()
            .map_err(|_| anyhow!("unrecognised timestamp {raw:?}"))
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);

    // Whatever follows the seconds is an optional fraction then the zone.
    let zone = raw[19..]
        .find(['Z', '+', '-'])
        .map(|i| &raw[19 + i..])
        .ok_or_else(|| anyhow!("timestamp {raw:?} has no time zone"))?;
    let offset = match zone.as_bytes()[0] {
        b'Z' => 0,
        sign => {
            if zone.len() < 6 {
                bail!("unrecognised time zone in {raw:?}");
            }
            let hours: i64 = zone[1..3]
                .parse()
                .map_err(|_| anyhow!("unrecognised time zone in {raw:?}"))?;
            let minutes: i64 = zone[4..6]
                .parse()
                .map_err(|_| anyhow!("unrecognised time zone in {raw:?}"))?;
            let magnitude = hours * 3600 + minutes * 60;
            if sign == b'-' { -magnitude } else { magnitude }
        }
    };

    let local = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Ok(format_utc(local - offset))
}

fn format_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let time = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_offset_timestamps_to_utc() {
        // The shape YouTube actually serves.
        assert_eq!(
            to_utc("2023-10-03T10:20:08.354091-07:00").unwrap(),
            "2023-10-03T17:20:08Z"
        );
        // Positive offsets, and one that rolls back over midnight.
        assert_eq!(
            to_utc("2024-02-29T01:30:00+02:00").unwrap(),
            "2024-02-28T23:30:00Z"
        );
        assert_eq!(to_utc("2020-01-01T00:00:00Z").unwrap(), "2020-01-01T00:00:00Z");
    }

    #[test]
    fn rejects_junk() {
        assert!(to_utc("yesterday").is_err());
        assert!(to_utc("2023-10-03 10:20:08").is_err());
        assert!(to_utc("2023-10-03T10:20:08").is_err()); // no zone
    }

    #[test]
    fn civil_date_conversions_round_trip() {
        for days in [0, 1, 19_000, -1, -719_162, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{days}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn finds_the_date_in_a_json_ld_block() {
        let html = r#"<script type="application/ld+json">{"@type":"https://schema.org/DiscussionForumPosting","url":"https://www.youtube.com/post/abc","datePublished":"2023-10-03T10:20:08.354091-07:00"}</script>"#;
        assert_eq!(
            extract_date_published(html),
            Some("2023-10-03T10:20:08.354091-07:00")
        );
        assert_eq!(extract_date_published("<html></html>"), None);
    }
}
