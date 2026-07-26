//! Locating and launching a Chromium-family browser over the DevTools protocol.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use headless_chrome::protocol::cdp::{Emulation, Runtime};
use headless_chrome::{Browser, LaunchOptions, Tab};
use log::{debug, trace};
use serde_json::Value;

/// Viewport used for rendering. Wide enough that YouTube uses its desktop
/// layout and tall enough that most posts fit without `captureBeyondViewport`
/// having to stitch anything together.
pub const VIEWPORT: (u32, u32) = (1400, 1800);

/// How long the browser may send us no events before the connection is torn
/// down. See the note in [`launch`]: this is deliberately far longer than any
/// run, because a quiet page is normal here rather than a sign of trouble.
const IDLE_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Finds a Chromium-based browser: `$CHROME_PATH` first, then the usual Chrome
/// and Edge install locations. Edge is fine here, it speaks the same protocol.
fn find_browser() -> Result<PathBuf> {
	if let Some(p) = std::env::var_os("CHROME_PATH") {
		let p = PathBuf::from(p);
		if p.exists() {
			return Ok(p);
		}
		debug!("CHROME_PATH points at {}, which does not exist; falling back", p.display());
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
	debug!("browser: {}", path.display());

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
		// Edge in particular spins up service workers for its own new-tab page.
		// The target churn is noise on the protocol connection, and long
		// sessions have fallen over on it.
		"--disable-background-networking",
		"--disable-component-update",
		"--disable-sync",
		"--no-first-run",
		"--no-default-browser-check",
	]
	.iter()
	.map(OsString::from)
	.collect();

	let options = LaunchOptions::default_builder()
		.path(Some(path))
		.headless(true)
		// The default is 30 seconds, and it counts *events* from the browser,
		// not the calls we make: a page that simply sits there — which is most
		// of what watching a chat looks like — has its connection dropped out
		// from under it on the half-minute. Everything here drives the page by
		// polling, so this wants to be effectively never.
		.idle_browser_timeout(IDLE_TIMEOUT)
		.window_size(Some(VIEWPORT))
		.args(args.iter().map(|a| a.as_os_str()).collect())
		.build()
		.context("failed to build launch options")?;
	trace!("launch args: {args:?}");

	Browser::new(options).context("failed to launch browser")
}

/// Evaluates `expr` in the page and returns its value, reporting any JavaScript
/// exception rather than silently yielding nothing.
pub fn eval(tab: &Tab, expr: &str) -> Result<Value> {
	let result = tab.call_method(Runtime::Evaluate {
		expression: expr.to_string(),
		return_by_value: Some(true),
		await_promise: Some(true),
		user_gesture: Some(true),
		silent: Some(false),
		generate_preview: Some(false),
		include_command_line_api: Some(false),
		object_group: None,
		context_id: None,
		throw_on_side_effect: None,
		timeout: None,
		disable_breaks: None,
		repl_mode: None,
		allow_unsafe_eval_blocked_by_csp: None,
		unique_context_id: None,
		serialization_options: None,
	})?;

	if let Some(details) = result.exception_details {
		let message = details.exception.and_then(|e| e.description).unwrap_or(details.text);
		bail!("JS error in `{}`: {message}", expr.lines().next().unwrap_or(expr));
	}
	Ok(result.result.value.unwrap_or(Value::Null))
}

/// Makes the page report a dark colour-scheme preference, which is what
/// YouTube's "use device theme" default follows.
pub fn emulate_dark(tab: &Tab) -> Result<()> {
	tab.call_method(Emulation::SetEmulatedMedia {
		media: Some("screen".to_string()),
		features: Some(vec![Emulation::MediaFeature {
			name: "prefers-color-scheme".to_string(),
			value: "dark".to_string(),
		}]),
	})?;
	Ok(())
}

/// Pins the layout viewport, which otherwise depends on the window's chrome, so
/// what we capture is identical across runs. Resizing reflows the page, so
/// callers that resize often should skip the no-op case themselves.
pub fn set_viewport(tab: &Tab, width: u32, height: u32) -> Result<()> {
	tab.call_method(Emulation::SetDeviceMetricsOverride {
		width,
		height,
		device_scale_factor: 1.0,
		mobile: false,
		scale: None,
		screen_width: None,
		screen_height: None,
		position_x: None,
		position_y: None,
		dont_set_visible_size: None,
		screen_orientation: None,
		viewport: None,
		display_feature: None,
		device_posture: None,
	})
	.context("resizing viewport")?;
	Ok(())
}

/// YouTube renders its theme server-side from this preference, so setting the
/// cookie and reloading beats fighting a light-theme first paint. The caller
/// must already be on a youtube.com page, and must reload afterwards.
pub fn set_dark_theme_cookie(tab: &Tab) -> Result<()> {
	eval(tab, "document.cookie = 'PREF=f6=400; domain=.youtube.com; path=/'")?;
	Ok(())
}
