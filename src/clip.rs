//! Turning one element of an injected page into a screenshot.
//!
//! The posts feed, a post's comment section and a video's comments are all
//! captured the same way: bring the element into view, wait for its images,
//! grow the viewport until it fits, then clip exactly it. The page-specific
//! half lives in the injected JavaScript, which each of them exposes as
//! `window.__yta` with the same `focus`/`align` shape, so everything below is
//! shared.

use std::cell::Cell;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use headless_chrome::Tab;
use headless_chrome::protocol::cdp::Page;
use log::{debug, trace, warn};
use serde_json::Value;

use crate::browser::{self, ImageFormat};

/// Tallest viewport we will grow to for one oversized element. Chromium gets
/// unhappy allocating capture surfaces much beyond this.
pub(crate) const MAX_CAPTURE_HEIGHT: u32 = 12000;

/// Viewport space kept free around an element: enough for the margin `align`
/// leaves above it, plus slack below.
const HEADROOM: u32 = 400;

/// How long an element that measures nothing is given to render before we give
/// up on it.
const EMPTY_GRACE: Duration = Duration::from_millis(1500);

/// Captures elements of whichever page `tab` is on, one at a time.
pub(crate) struct Clipper<'a> {
	tab: &'a Tab,
	/// What the screenshots are encoded as.
	format: ImageFormat,
	/// Current layout viewport height, to avoid redundant reflows.
	viewport_height: Cell<u32>,
}

impl<'a> Clipper<'a> {
	pub(crate) fn new(tab: &'a Tab, format: ImageFormat) -> Self {
		Self {
			tab,
			format,
			viewport_height: Cell::new(0),
		}
	}

	pub(crate) fn eval(&self, expr: &str) -> Result<Value> {
		browser::eval(self.tab, expr)
	}

	/// Whether the page is still reachable, i.e. the browser has not crashed.
	pub(crate) fn is_alive(&self) -> bool {
		self.eval("1").is_ok()
	}

	/// Pins the layout viewport, which otherwise depends on the window's
	/// chrome, so element widths are identical across runs.
	pub(crate) fn init_viewport(&self) -> Result<()> {
		self.set_viewport_height(browser::VIEWPORT.1)
	}

	/// Scrolls element `i` into view, waits for its images, and clips it.
	///
	/// The element is captured entirely within the viewport — the viewport is
	/// grown to fit a tall one rather than relying on `captureBeyondViewport`,
	/// which reflows YouTube's lazily rendered feed and misaligns the clip.
	/// Returns the encoded image alongside the settled `align` info, whose
	/// `meta` is what the caller indexes the capture by.
	pub(crate) fn capture(&self, i: usize) -> Result<(Vec<u8>, Value)> {
		let info = self.focus_until_loaded(i)?;

		// The rect is padded, so an element that has collapsed to nothing still
		// measures a clippable square; the page says outright when there is
		// nothing inside it, which would otherwise be captured as blank.
		if info["empty"].as_bool().unwrap_or(false) {
			bail!("element occupies no space on the page");
		}
		let height = info["rect"]["height"].as_f64().unwrap_or(0.0);
		if height <= 0.0 {
			bail!("element has zero height");
		}
		if height.ceil() as u32 + HEADROOM > MAX_CAPTURE_HEIGHT {
			bail!("element is {height:.0}px tall, beyond the capture limit");
		}
		self.fit_viewport(height)?;

		// Re-measure after the resize, immediately before capturing. If the
		// element still runs off the bottom — near the end of the page there may
		// not be enough left to scroll — grow to exactly what it needs.
		let mut info = self.align(i)?;
		let overflow = info["viewTop"].as_f64().unwrap_or(0.0) + height - info["viewHeight"].as_f64().unwrap_or(0.0);
		if overflow > 0.0 {
			debug!("element {} overflows the viewport by {overflow:.0}px, growing", i + 1);
			self.fit_viewport(height + overflow)?;
			info = self.align(i)?;
		}

		let rect = &info["rect"];
		let clip = Page::Viewport {
			x: rect["x"].as_f64().unwrap_or(0.0),
			y: rect["y"].as_f64().unwrap_or(0.0),
			width: rect["width"].as_f64().unwrap_or(0.0),
			height: rect["height"].as_f64().unwrap_or(0.0),
			scale: 2.0, // capture at 2x for legible text
		};

		let image = browser::capture(self.tab, clip, self.format)?;
		debug!(
			"element {} captured, {:.0}x{:.0}px, {} KiB",
			i + 1,
			rect["width"].as_f64().unwrap_or(0.0),
			rect["height"].as_f64().unwrap_or(0.0),
			image.len() / 1024
		);
		Ok((image, info))
	}

	/// Brings element `i` into view and waits until its images have decoded, so
	/// one with an attachment is never captured half-blank.
	fn focus_until_loaded(&self, i: usize) -> Result<Value> {
		let deadline = Instant::now() + Duration::from_secs(20);
		// An element with nothing in it is given a moment to fill in — one only
		// just scrolled to can be a frame behind — but not the whole wait, since
		// one that is still empty after that is not going to render at all.
		let render_deadline = Instant::now() + EMPTY_GRACE;
		loop {
			let info = self.eval(&format!("__yta.focus({i})"))?;
			if !info["exists"].as_bool().unwrap_or(false) {
				bail!("element {i} disappeared from the page");
			}
			if info["empty"].as_bool().unwrap_or(false) {
				if Instant::now() > render_deadline {
					// Handed back rather than raised here, so the one place that
					// refuses an empty element is the caller.
					return Ok(info);
				}
			} else if info["ready"].as_bool().unwrap_or(false) {
				// A beat for expanded text and just-decoded images to paint,
				// then measure the settled layout.
				sleep(Duration::from_millis(400));
				return self.eval(&format!("__yta.focus({i})"));
			}
			if Instant::now() > deadline {
				warn!("element {} images still loading, capturing anyway", i + 1);
				return Ok(info);
			}
			sleep(Duration::from_millis(250));
		}
	}

	fn align(&self, i: usize) -> Result<Value> {
		let info = self.eval(&format!("__yta.align({i})"))?;
		if !info["exists"].as_bool().unwrap_or(false) {
			bail!("element {i} disappeared from the page");
		}
		if info["empty"].as_bool().unwrap_or(false) {
			bail!("element {i} collapsed to nothing while being captured");
		}
		Ok(info)
	}

	/// Resizes the layout viewport, keeping the width fixed so YouTube's
	/// desktop layout and column widths do not change between captures.
	/// Resizing reflows the page, so this is a no-op when the height already
	/// matches.
	fn set_viewport_height(&self, height: u32) -> Result<()> {
		if self.viewport_height.get() == height {
			return Ok(());
		}
		browser::set_viewport(self.tab, browser::VIEWPORT.0, height)?;
		trace!("viewport height {} -> {height}", self.viewport_height.get());
		self.viewport_height.set(height);
		sleep(Duration::from_millis(250)); // let the reflow settle
		Ok(())
	}

	/// Sizes the viewport so an element of `height` fits with room for the
	/// margin `align` leaves above it.
	fn fit_viewport(&self, height: f64) -> Result<()> {
		let needed = height.ceil() as u32 + HEADROOM;
		self.set_viewport_height(needed.max(browser::VIEWPORT.1).min(MAX_CAPTURE_HEIGHT))
	}
}
