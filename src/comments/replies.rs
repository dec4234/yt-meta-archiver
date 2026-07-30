//! Opening a comment thread and reading the replies inside it.
//!
//! A thread reads the same wherever comments hang, so this is shared with the
//! community-post extractor exactly as [`super::meta`] and the injected script
//! are: both extractors capture a thread a comment at a time, the top-level
//! comment as its own clip and each kept reply as another, and both address
//! what they are capturing through [`select`] rather than by position, since a
//! reply arriving in an earlier thread slides every later comment along.

use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use log::trace;

use super::meta::{self, Meta};
use crate::clip::Clipper;

/// Times we will ask one thread for another page of replies. YouTube hands them
/// over ten or so at a time, which is already more than a run usually asks for.
const MAX_ROUNDS: u32 = 12;

/// How long a page of replies is given to arrive before the thread is asked
/// again.
const WAIT: Duration = Duration::from_millis(600);

/// Rounds that must find no new reply before a thread is called fully loaded.
const STABLE_ROUNDS: u32 = 3;

/// Clicks thread `i` open and keeps asking for more replies until `limit` of
/// them are on the page, or the thread has no more to give. Returns how many are
/// loaded, which the last round can leave larger than `limit` — YouTube hands
/// them over a page at a time.
pub(crate) fn load(clipper: &Clipper, i: usize, limit: usize) -> Result<usize> {
	let mut loaded = 0;
	let mut stable = 0;
	for _ in 0..MAX_ROUNDS {
		let shown = clipper.eval(&format!("__yta.showReplies({i})"))?;
		let count = shown["count"].as_u64().unwrap_or(0) as usize;
		let more = shown["more"].as_bool().unwrap_or(false);
		if count > loaded {
			trace!("comment {} has {count} replies loaded...", i + 1);
			loaded = count;
			stable = 0;
		} else {
			stable += 1;
		}
		// Enough to satisfy the caller; the rest of the thread is not our
		// problem.
		if loaded >= limit {
			return Ok(loaded);
		}
		// Nothing was left to click, and what the click before it asked for has
		// arrived: this is the whole thread. A count of zero here is instead a
		// thread whose replies are still in flight — the click that opened it
		// necessarily found nothing more to click — so it is waited on rather
		// than written off.
		if !more && loaded > 0 {
			return Ok(loaded);
		}
		// A thread that has stopped growing is done whether or not it still
		// offers something to click: its continuation stays in the markup even
		// once it has no further page to hand over.
		if stable >= STABLE_ROUNDS {
			break;
		}
		sleep(WAIT);
	}
	Ok(loaded)
}

/// Thread `i`'s loaded replies, in the order the page shows them.
pub(crate) fn metas(clipper: &Clipper, i: usize) -> Result<Vec<Meta>> {
	let list = clipper.eval(&format!("__yta.replyMetas({i})"))?;
	Ok(list.as_array().map(|list| list.iter().map(Meta::from_value).collect()).unwrap_or_default())
}

/// Aims the next capture at thread `i`'s comment (`reply` absent) or at its
/// `reply`th reply, returning where it sits in the list of capturable elements,
/// or `None` when the page no longer has it.
pub(crate) fn select(clipper: &Clipper, i: usize, reply: Option<usize>) -> Result<Option<usize>> {
	let r = reply.map_or(-1, |r| r as i64);
	let at = clipper.eval(&format!("__yta.select({i}, {r})"))?.as_i64().unwrap_or(-1);
	Ok(usize::try_from(at).ok())
}

/// `001_UgwXyZ`, the folder a comment's replies go in: the same name as the
/// comment's own screenshot, so the two sit together when the directory it was
/// captured into is sorted.
pub(crate) fn dir_name(ordinal: usize, parent_id: Option<&str>) -> String {
	format!("{:03}_{}", ordinal + 1, meta::sanitize(parent_id))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn puts_a_threads_replies_in_a_folder_named_after_it() {
		// The comment itself is `001_UgwXyZ.png`, so its replies land in
		// `001_UgwXyZ/` beside it.
		assert_eq!(dir_name(0, Some("UgwXyZ")), "001_UgwXyZ");
		assert_eq!(dir_name(4, Some("Ugw/../Abc")), "005_UgwAbc");
		assert_eq!(dir_name(11, None), "012_unknown");
	}
}
