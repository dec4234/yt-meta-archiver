//! Naming channels: what an input names, the key its images are filed under,
//! and the run shared by the commands that take a list of channels and save
//! one image for each.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use log::info;

use crate::cli;
use crate::image_store::ImageStore;

/// A channel id: `UC` and 22 characters of the URL-safe base64 alphabet.
const CHANNEL_ID_LEN: usize = 24;

/// The channels to work on and how, shared by every command that takes a list
/// of them.
#[derive(clap::Args)]
pub struct ListArgs {
	/// Channels: a handle (@name), a channel id (UC…), or a channel URL
	channels: Vec<String>,

	/// Also read channels from this file, one per line. Blank lines and lines
	/// that start with # are skipped.
	#[arg(long, value_name = "FILE")]
	from: Option<PathBuf>,

	/// Download every image again, over what an earlier run saved.
	#[arg(long)]
	pub refresh: bool,

	/// Fetch several channels at once. Takes the number of workers, defaulting
	/// to one per core up to 4.
	#[arg(long, value_name = "WORKERS", num_args = 0..=1, default_missing_value = "0")]
	mt: Option<usize>,
}

/// What a channel input names.
#[derive(Debug, PartialEq)]
pub enum Channel {
	/// A bare channel id, filed under itself.
	Id(String),
	/// A channel page, filed under the id or handle in its URL.
	Url(String),
}

impl Channel {
	/// A channel id is taken as is, a URL is used as given, and anything else is
	/// a handle, with or without its `@`.
	pub fn parse(input: &str) -> Self {
		let input = input.trim().trim_end_matches('/');
		if input.starts_with("http") {
			return Self::Url(input.to_string());
		}
		if input.len() == CHANNEL_ID_LEN && input.starts_with("UC") && input.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
			return Self::Id(input.to_string());
		}
		let handle = input.strip_prefix('@').unwrap_or(input);
		Self::Url(format!("https://www.youtube.com/@{handle}"))
	}

	/// The channel's own page.
	pub fn page_url(&self) -> String {
		match self {
			Self::Id(id) => format!("https://www.youtube.com/channel/{id}"),
			Self::Url(url) => url.clone(),
		}
	}

	/// What the channel's images are filed under, or `None` for a URL that names
	/// neither a channel id nor a handle.
	pub fn key(&self) -> Option<String> {
		match self {
			Self::Id(id) => sanitize(id),
			Self::Url(url) => key_from_url(url),
		}
	}
}

/// Runs `grab` over every channel `args` names, prints one `input<TAB>path`
/// line per image found, and fails if any channel came back without one.
pub fn grab_each(args: &ListArgs, store: &ImageStore, grab: impl Fn(&Channel) -> Option<String> + Sync) -> Result<()> {
	let channels = channels(args)?;
	if channels.is_empty() {
		bail!("no channels given (pass them as arguments or with --from)");
	}
	let workers = args.mt.map_or(1, cli::worker_count).min(channels.len());
	info!("channels: {}", channels.len());
	info!("output:   {}", store.dir().display());
	if workers > 1 {
		info!("workers:  {workers}");
	}

	let found = in_parallel(&channels, workers, |input| grab(&Channel::parse(input)));

	// One line per channel on stdout, so the result can be piped elsewhere.
	let mut missing = 0;
	for (channel, path) in channels.iter().zip(&found) {
		match path {
			Some(path) => println!("{channel}\t{path}"),
			None => missing += 1,
		}
	}

	let (saved, reused) = store.counts();
	let what = store.what();
	info!("{what}s: {saved} saved, {reused} reused, {missing} missing, in {}", store.dir().display());
	if missing > 0 {
		bail!("{missing} of {} channels had no {what}", channels.len());
	}
	Ok(())
}

/// The channels named on the command line, then those in `--from`.
fn channels(args: &ListArgs) -> Result<Vec<String>> {
	let mut channels: Vec<String> = args.channels.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
	if let Some(file) = &args.from {
		let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
		channels.extend(text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_string));
	}
	Ok(channels)
}

/// `work` over every input on `workers` threads, the results in input order.
fn in_parallel<T: Send>(inputs: &[String], workers: usize, work: impl Fn(&str) -> T + Sync) -> Vec<T> {
	if workers <= 1 {
		return inputs.iter().map(|input| work(input)).collect();
	}

	let next = AtomicUsize::new(0);
	let mut done: Vec<(usize, T)> = std::thread::scope(|scope| {
		let handles: Vec<_> = (0..workers)
			.map(|_| {
				scope.spawn(|| {
					let mut mine = Vec::new();
					loop {
						let i = next.fetch_add(1, Ordering::Relaxed);
						let Some(input) = inputs.get(i) else {
							break mine;
						};
						mine.push((i, work(input)));
					}
				})
			})
			.collect();
		handles.into_iter().flat_map(|h| h.join().expect("channel worker panicked")).collect()
	});
	done.sort_by_key(|(i, _)| *i);
	done.into_iter().map(|(_, result)| result).collect()
}

/// The key a channel URL files its images under: the channel id from a
/// `/channel/UC…` URL, the `@handle` from a handle URL.
pub fn key_from_url(url: &str) -> Option<String> {
	let path = url.split(['?', '#']).next()?;
	let mut segments = path.split('/').skip_while(|s| !s.starts_with('@') && *s != "channel");
	let key = match segments.next()? {
		"channel" => segments.next()?,
		handle => handle,
	};
	sanitize(key)
}

/// A key reduced to what is safe in a file name.
pub fn sanitize(key: &str) -> Option<String> {
	let key: String = key.chars().filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '%')).collect();
	(!key.is_empty() && key != "." && key != "..").then_some(key)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_handles_ids_and_urls() {
		assert_eq!(Channel::parse("@Someone"), Channel::Url("https://www.youtube.com/@Someone".to_string()));
		assert_eq!(Channel::parse("Someone"), Channel::Url("https://www.youtube.com/@Someone".to_string()));
		assert_eq!(Channel::parse("UCabcdefghijklmnopqrstuv"), Channel::Id("UCabcdefghijklmnopqrstuv".to_string()));
		assert_eq!(
			Channel::parse("https://www.youtube.com/@Someone/videos/"),
			Channel::Url("https://www.youtube.com/@Someone/videos".to_string())
		);
	}

	#[test]
	fn a_short_uc_name_is_a_handle_not_an_id() {
		assert_eq!(Channel::parse("UCLA"), Channel::Url("https://www.youtube.com/@UCLA".to_string()));
	}

	#[test]
	fn an_id_and_its_url_share_a_key_and_a_page() {
		let id = Channel::parse("UCabcdefghijklmnopqrstuv");
		let url = Channel::parse("https://www.youtube.com/channel/UCabcdefghijklmnopqrstuv");
		assert_eq!(id.key(), url.key());
		assert_eq!(id.page_url(), url.page_url());
	}

	#[test]
	fn files_a_channel_under_its_id_or_handle() {
		assert_eq!(key_from_url("https://www.youtube.com/@Someone").as_deref(), Some("@Someone"));
		assert_eq!(key_from_url("https://www.youtube.com/@Someone/posts").as_deref(), Some("@Someone"));
		assert_eq!(key_from_url("https://www.youtube.com/channel/UC123abc?x=1").as_deref(), Some("UC123abc"));
		assert_eq!(key_from_url("https://www.youtube.com/watch?v=abc"), None);
	}

	#[test]
	fn keeps_only_what_is_safe_in_a_file_name() {
		assert_eq!(sanitize("@a/../b").as_deref(), Some("@a..b"));
		assert_eq!(sanitize("..").as_deref(), None);
		assert_eq!(sanitize("").as_deref(), None);
	}

	fn list_args(channels: &[&str], from: Option<PathBuf>) -> ListArgs {
		ListArgs {
			channels: channels.iter().map(|c| c.to_string()).collect(),
			from,
			refresh: false,
			mt: None,
		}
	}

	#[test]
	fn reads_channels_from_the_arguments_then_the_file() {
		let dir = std::env::temp_dir().join(format!("yta-channel-list-test-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let file = dir.join("channels.txt");
		std::fs::write(&file, "# a comment\n\n  @fromfile  \nUCabcdefghijklmnopqrstuv\n").unwrap();

		let args = list_args(&[" @first ", "", "@second"], Some(file));
		assert_eq!(channels(&args).unwrap(), vec!["@first", "@second", "@fromfile", "UCabcdefghijklmnopqrstuv"]);

		std::fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn a_missing_list_file_is_an_error() {
		let args = list_args(&[], Some(PathBuf::from("this/file/does/not/exist.txt")));
		assert!(channels(&args).is_err());
	}

	#[test]
	fn grabbing_with_no_channels_is_an_error() {
		let store = ImageStore::new(std::path::Path::new("."), "test image", false);
		let err = grab_each(&list_args(&[], None), &store, |_| None).unwrap_err();
		assert!(err.to_string().contains("no channels given"), "{err}");
	}

	#[test]
	fn grabbing_fails_when_a_channel_has_no_image() {
		let store = ImageStore::new(std::path::Path::new("."), "test image", false);
		let args = list_args(&["@has", "@lacks"], None);
		let err = grab_each(&args, &store, |channel| (channel.key().as_deref() == Some("@has")).then(|| "has.png".to_string())).unwrap_err();
		assert!(err.to_string().contains("1 of 2"), "{err}");
		assert!(grab_each(&list_args(&["@has"], None), &store, |_| Some("has.png".to_string())).is_ok());
	}

	#[test]
	fn a_single_worker_keeps_the_input_order_too() {
		let inputs: Vec<String> = vec!["b".into(), "a".into()];
		assert_eq!(in_parallel(&inputs, 1, str::to_uppercase), vec!["B", "A"]);
		assert!(in_parallel(&[], 3, |s: &str| s.len()).is_empty());
	}

	#[test]
	fn keeps_the_input_order_across_workers() {
		let inputs: Vec<String> = (0..20).map(|i| i.to_string()).collect();
		let out = in_parallel(&inputs, 4, |s| s.parse::<u32>().unwrap());
		assert_eq!(out, (0..20).collect::<Vec<_>>());
	}
}
