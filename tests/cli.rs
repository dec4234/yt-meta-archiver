//! The built command, run as a user would run it. Only the paths that end
//! before any request is made are run here, so these tests need no network and
//! no browser.

mod common;

use std::process::{Command, Output};

use common::TempDir;

fn run(args: &[&str]) -> Output {
	Command::new(env!("CARGO_BIN_EXE_yt-meta-archiver"))
		.args(args)
		.env_remove("RUST_LOG")
		.output()
		.expect("the command runs")
}

fn stdout(output: &Output) -> String {
	String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
	String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_lists_every_subcommand() {
	let output = run(&["--help"]);
	assert!(output.status.success());
	let help = stdout(&output);
	for subcommand in ["posts", "livechat", "comments", "pfp", "banner"] {
		assert!(help.contains(subcommand), "{subcommand} missing from:\n{help}");
	}
}

#[test]
fn prints_its_version() {
	let output = run(&["--version"]);
	assert!(output.status.success());
	assert!(stdout(&output).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn no_subcommand_is_a_usage_error() {
	let output = run(&[]);
	assert!(!output.status.success());
	assert!(stderr(&output).contains("Usage"), "{}", stderr(&output));
}

#[test]
fn comments_wants_a_video_not_a_channel() {
	let output = run(&["comments", "@SomeChannel"]);
	assert!(!output.status.success());
	assert!(stderr(&output).contains("does not name a video"), "{}", stderr(&output));
}

#[test]
fn image_grabbers_want_at_least_one_channel() {
	for subcommand in ["pfp", "banner"] {
		let out = TempDir::new(&format!("cli-{subcommand}"));
		let output = run(&[subcommand, "--out", out.path().to_str().unwrap()]);
		assert!(!output.status.success(), "{subcommand}");
		assert!(stderr(&output).contains("no channels given"), "{subcommand}: {}", stderr(&output));
		// The output directory is still made, ready for the next run.
		assert!(out.path().is_dir());
	}
}

#[test]
fn a_missing_channel_list_file_is_reported() {
	let out = TempDir::new("cli-from");
	let output = run(&["pfp", "--from", "no/such/channels.txt", "--out", out.path().to_str().unwrap()]);
	assert!(!output.status.success());
	assert!(stderr(&output).contains("channels.txt"), "{}", stderr(&output));
}

#[test]
fn an_out_of_range_picture_size_is_refused() {
	let output = run(&["pfp", "@a", "--size", "5000"]);
	assert!(!output.status.success());
	assert!(stderr(&output).contains("5000"), "{}", stderr(&output));
}
