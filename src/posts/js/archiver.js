// Injected into the channel's posts page. Everything the Rust side needs from
// the DOM is exposed on `window.__yta` and returns JSON-serialisable values.
(() => {
    const PAD = 12; // px of page background left around each captured post

    // Posts are parked this far down the viewport rather than flush with the
    // top, so YouTube's sticky channel header is never painted over them.
    const TOP_MARGIN = 220;

    // YouTube's own CSS keys off `html[dark]`, so setting the attribute forces
    // the dark palette regardless of what the appearance setting resolved to.
    function forceDarkTheme() {
        document.documentElement.setAttribute("dark", "");
        const app = document.querySelector("ytd-app");
        if (app) app.setAttribute("is-dark-theme", "");
    }

    // Fixed-position chrome would otherwise be painted over the clipped region.
    function hideOverlays() {
        const selectors = [
            "#masthead-container",
            "ytd-consent-bump-v2-lightbox",
            "tp-yt-iron-overlay-backdrop",
            "ytd-popup-container",
            "ytd-mealbar-promo-renderer",
            "yt-mealbar-promo-renderer",
            "#guide",
            "#scrim",
        ];
        for (const sel of selectors) {
            for (const el of document.querySelectorAll(sel)) {
                el.style.setProperty("display", "none", "important");
            }
        }
    }

    // The thread wrapper is what carries the full post card; the layout differs
    // between the list and grid renderings of the posts tab.
    function threads() {
        let found = [...document.querySelectorAll("ytd-backstage-post-thread-renderer")];
        if (found.length === 0) {
            found = [...document.querySelectorAll("ytd-rich-item-renderer")].filter((el) =>
                el.querySelector("ytd-backstage-post-renderer, ytd-shared-post-renderer"),
            );
        }
        return found;
    }

    // The card itself, i.e. what gets captured. Excludes the thread wrapper's
    // outer padding and dividers.
    function card(thread) {
        return (
            thread.querySelector("ytd-shared-post-renderer, ytd-backstage-post-renderer") || thread
        );
    }

    function visible(el) {
        return el.offsetParent !== null && el.getBoundingClientRect().height > 0;
    }

    // Truncated post bodies hide their tail behind a "Read more" button.
    function expand(root) {
        let clicked = 0;
        for (const el of root.querySelectorAll("#more")) {
            if (visible(el)) {
                el.click();
                clicked++;
            }
        }
        return clicked;
    }

    // A post is ready to capture once every image it displays has decoded;
    // YouTube loads attachment images lazily as they approach the viewport.
    function imagesReady(root) {
        for (const img of root.querySelectorAll("img")) {
            if (!img.src || !visible(img)) continue;
            if (!img.complete || img.naturalWidth === 0) return false;
        }
        return true;
    }

    // Identity and publication details of a post. The permalink and the
    // timestamp live on the same anchor in the post header.
    function postMeta(root) {
        const link = root.querySelector('a[href*="/post/"]');
        const id = link
            ? (link.getAttribute("href").match(/\/post\/([A-Za-z0-9_-]+)/) || [])[1] || null
            : null;

        // Community posts only ever carry a relative time ("2 years ago") —
        // there is no exact date anywhere in the feed markup.
        const stamp = root.querySelector("#published-time-text") || link;

        return {
            id,
            published: stamp ? stamp.textContent.trim() : null,
        };
    }

    // Page (not viewport) coordinates, padded, so the clip stays valid under
    // `captureBeyondViewport`.
    function paddedRect(el) {
        const r = el.getBoundingClientRect();
        const x = Math.max(0, r.left + window.scrollX - PAD);
        const y = Math.max(0, r.top + window.scrollY - PAD);
        return {
            x,
            y,
            width: Math.min(r.width + PAD * 2, document.documentElement.scrollWidth - x),
            height: r.height + PAD * 2,
        };
    }

    window.__yta = {
        prepare() {
            forceDarkTheme();
            hideOverlays();
            return threads().length;
        },

        count() {
            return threads().length;
        },

        // One step of the infinite-scroll loop.
        scrollToBottom() {
            hideOverlays();
            window.scrollTo(0, document.documentElement.scrollHeight);
            return threads().length;
        },

        // Brings post `i` into view and reports whether it can be captured yet.
        focus(i) {
            const list = threads();
            if (i >= list.length) return {exists: false};
            const el = card(list[i]);
            el.scrollIntoView({block: "center", behavior: "instant"});
            hideOverlays();
            expand(el);
            return {
                exists: true,
                ready: imagesReady(el),
                meta: postMeta(el),
                rect: paddedRect(el),
            };
        },

        // Parks post `i` just below the sticky header and re-measures it.
        // Called once the viewport has been sized to fit, immediately before
        // the capture, so the clip matches what is actually painted.
        // `viewTop`/`viewHeight` let the caller confirm the post really is
        // fully on screen, since the clip cannot reach past the viewport.
        align(i) {
            const list = threads();
            if (i >= list.length) return {exists: false};
            const el = card(list[i]);
            hideOverlays();
            const top = el.getBoundingClientRect().top + window.scrollY;
            window.scrollTo(0, Math.max(0, top - TOP_MARGIN));

            const rect = paddedRect(el);
            return {
                exists: true,
                ready: imagesReady(el),
                meta: postMeta(el),
                rect,
                viewTop: rect.y - window.scrollY,
                viewHeight: window.innerHeight,
            };
        },
    };

    return true;
})();
