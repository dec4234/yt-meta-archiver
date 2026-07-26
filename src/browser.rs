//! Locating and launching a Chromium-family browser over the DevTools protocol.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use headless_chrome::{Browser, LaunchOptions};

/// Viewport used for rendering. Wide enough that YouTube uses its desktop
/// layout and tall enough that most posts fit without `captureBeyondViewport`
/// having to stitch anything together.
pub const VIEWPORT: (u32, u32) = (1400, 1800);

/// Finds a Chromium-based browser: `$CHROME_PATH` first, then the usual Chrome
/// and Edge install locations. Edge is fine here, it speaks the same protocol.
fn find_browser() -> Result<PathBuf> {
	if let Some(p) = std::env::var_os("CHROME_PATH") {
		let p = PathBuf::from(p);
		if p.exists() {
			return Ok(p);
		}
	}

	let program_files = std::env::var("ProgramFiles").unwrap_or_default();
	let program_files_x86 = std::env::var("ProgramFiles(x86)").unwrap_or_default();
	let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();

	let candidates = [
		format!(r"{program_files}\Google\Chrome\Application\chrome.exe"),
		format!(r"{program_files_x86}\Google\Chrome\Application\chrome.exe"),
		format!(r"{local_app_data}\Google\Chrome\Application\chrome.exe"),
		format!(r"{program_files}\Microsoft\Edge\Application\msedge.exe"),
		format!(r"{program_files_x86}\Microsoft\Edge\Application\msedge.exe"),
		"/usr/bin/google-chrome".to_string(),
		"/usr/bin/chromium".to_string(),
		"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string(),
	];

	candidates
		.iter()
		.map(PathBuf::from)
		.find(|p| p.exists())
		.context("no Chrome/Chromium/Edge binary found; set CHROME_PATH to one")
}

pub fn launch() -> Result<Browser> {
	let path = find_browser()?;
	eprintln!("browser: {}", path.display());

	// Owned so the borrowed `&OsStr` args outlive the builder.
	let args: Vec<OsString> = [
		"--hide-scrollbars",
		// Subpixel antialiasing leaves coloured fringes on glyphs once the
		// capture is scaled; grayscale AA keeps the text clean.
		"--disable-lcd-text",
		"--lang=en-US",
		"--mute-audio",
		"--disable-blink-features=AutomationControlled",
		"--autoplay-policy=user-gesture-required",
	]
	.iter()
	.map(OsString::from)
	.collect();

	let options = LaunchOptions::default_builder()
		.path(Some(path))
		.headless(true)
		.window_size(Some(VIEWPORT))
		.args(args.iter().map(|a| a.as_os_str()).collect())
		.build()
		.context("failed to build launch options")?;

	Browser::new(options).context("failed to launch browser")
}
