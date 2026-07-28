// Injected into a page whose comment section we want — a single community
// post, or a video's watch page. The comment markup is the same on both, so
// one script serves them; only the chrome to get out of the way differs.
//
// Exposes the same `window.__yta` surface as the feed archiver —
// `prepare`/`count`/`focus`/`align` — plus `metas()`, which reads every
// top-level comment at once so the caller can decide what is worth capturing
// before paying for any screenshots, and `likeCounts()`, the cheap slice of
// that used to decide when enough has been loaded.
(() => {
    const PAD = 12; // px of page background left around each captured comment

    // Comments are parked this far down the viewport rather than flush with
    // the top, so YouTube's sticky masthead is never painted over them.
    const TOP_MARGIN = 220;

    // YouTube's own CSS keys off `html[dark]`, so setting the attribute forces
    // the dark palette regardless of what the appearance setting resolved to.
    function forceDarkTheme() {
        document.documentElement.setAttribute("dark", "");
        const app = document.querySelector("ytd-app");
        if (app) app.setAttribute("is-dark-theme", "");
    }

    // Fixed-position chrome would otherwise be painted over the clipped region.
    // The watch page's player is in here too: scrolling past it pins it to a
    // corner over the comments, and we never capture it, so the whole container
    // goes rather than us fighting where it sticks. The comments live in a
    // separate column, so removing it costs their layout nothing.
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
            "#player-container-outer",
            "ytd-miniplayer",
        ];
        for (const sel of selectors) {
            for (const el of document.querySelectorAll(sel)) {
                el.style.setProperty("display", "none", "important");
            }
        }
    }

    // A watch page keeps playing whether or not its player is on screen, which
    // buys us nothing and costs the renderer plenty over a long comment run.
    function stopPlayback() {
        for (const video of document.querySelectorAll("video")) {
            video.pause();
        }
    }

    // One entry per top-level comment. The thread wrapper also holds the
    // replies, which are deliberately not what we capture.
    function threads() {
        return [...document.querySelectorAll("ytd-comment-thread-renderer")];
    }

    // The top-level comment inside a thread, i.e. what gets captured: the
    // reply list hangs off the thread beside it and is left out.
    function comment(thread) {
        return (
            thread.querySelector(
                "#comment > ytd-comment-view-model, #comment > ytd-comment-renderer, #comment",
            ) || thread
        );
    }

    function visible(el) {
        return el.offsetParent !== null && el.getBoundingClientRect().height > 0;
    }

    function text(el, sel) {
        const found = el.querySelector(sel);
        const value = found ? found.textContent.trim() : "";
        return value === "" ? null : value;
    }

    // Badges like the pinned marker and the creator heart are always in the
    // markup as empty containers, filled in only for the comments that have
    // them, so their presence says nothing on its own.
    function filled(el, sel) {
        const found = el.querySelector(sel);
        return !!found && (found.children.length > 0 || found.textContent.trim() !== "");
    }

    // Truncated comment bodies hide their tail behind a "Read more" button.
    function expand(root) {
        for (const el of root.querySelectorAll("#more, tp-yt-paper-button#expand")) {
            if (visible(el)) el.click();
        }
    }

    // A comment is ready to capture once every image it displays — the author
    // avatar, and any emoji or attached image — has decoded.
    function imagesReady(root) {
        for (const img of root.querySelectorAll("img")) {
            if (!img.src || !visible(img)) continue;
            if (!img.complete || img.naturalWidth === 0) return false;
        }
        return true;
    }

    // Everything the index records about one comment. Counts are handed back
    // as the strings YouTube renders ("1.2K"), which the caller parses; the
    // aria-label carries the exact number often enough to be worth preferring.
    function commentMeta(root, thread) {
        const author = root.querySelector("#author-text");
        // The timestamp is a link to the comment itself, whose `lc` parameter
        // is the only place its id appears; the span around it is the
        // fallback for a layout that renders the time as plain text.
        const time =
            root.querySelector("#published-time-text a") ||
            root.querySelector("#published-time-text");
        const href = time ? time.getAttribute("href") : null;
        const likes = root.querySelector("#vote-count-middle");

        return {
            id: href ? (href.match(/[?&]lc=([^&]+)/) || [])[1] || null : null,
            author: author ? author.textContent.trim() : null,
            authorUrl: author && author.href ? author.href : null,
            text: text(root, "#content-text"),
            likes: likes ? likes.textContent.trim() : "",
            likesLabel: likes ? likes.getAttribute("aria-label") : null,
            published: time ? time.textContent.trim() : null,
            replies: text(thread || root, "#replies #more-replies"),
            pinned: filled(root, "#pinned-comment-badge"),
            hearted: filled(root, "#creator-heart"),
        };
    }

    // Page (not viewport) coordinates, padded.
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
            stopPlayback();
            return threads().length;
        },

        count() {
            return threads().length;
        },

        // Whether the page has a comment section at all, as opposed to one
        // that simply has not loaded yet: a post with comments turned off says
        // so in place of the list.
        disabled() {
            const message = document.querySelector("ytd-message-renderer");
            return threads().length === 0 && !!message && visible(message);
        },

        // One step of the infinite-scroll loop: park the end of the comment
        // section just inside the viewport, which is what YouTube watches to
        // decide it should load more.
        //
        // Not the bottom of the page: a watch page's recommendation rail grows
        // as it is scrolled through, so the page end runs away from the
        // comments and the section never comes into view at all.
        scrollToBottom() {
            hideOverlays();
            stopPlayback();
            const section = document.querySelector("#comments");
            const end = section
                ? section.getBoundingClientRect().bottom + window.scrollY
                : document.documentElement.scrollHeight;
            window.scrollTo(0, Math.max(0, end - window.innerHeight * 0.9));
            return threads().length;
        },

        // Every top-level comment's details, in the order the page shows them.
        // Reading them all up front means the caller can filter before it
        // starts screenshotting.
        metas() {
            return threads().map((thread) => commentMeta(comment(thread), thread));
        },

        // Just the like counts, as the two strings the caller parses. Asked for
        // once per scroll round to tell how many comments already clear the
        // threshold, so it is deliberately a fraction of what `metas` costs.
        likeCounts() {
            return threads().map((thread) => {
                const likes = comment(thread).querySelector("#vote-count-middle");
                return {
                    likes: likes ? likes.textContent.trim() : "",
                    likesLabel: likes ? likes.getAttribute("aria-label") : null,
                };
            });
        },

        // Brings comment `i` into view and reports whether it can be captured.
        focus(i) {
            const list = threads();
            if (i >= list.length) return {exists: false};
            const el = comment(list[i]);
            el.scrollIntoView({block: "center", behavior: "instant"});
            hideOverlays();
            expand(el);
            return {
                exists: true,
                ready: imagesReady(el),
                meta: commentMeta(el, list[i]),
                rect: paddedRect(el),
            };
        },

        // Parks comment `i` just below the sticky masthead and re-measures it.
        // Called once the viewport has been sized to fit, immediately before
        // the capture, so the clip matches what is actually painted.
        align(i) {
            const list = threads();
            if (i >= list.length) return {exists: false};
            const el = comment(list[i]);
            hideOverlays();
            const top = el.getBoundingClientRect().top + window.scrollY;
            window.scrollTo(0, Math.max(0, top - TOP_MARGIN));

            const rect = paddedRect(el);
            return {
                exists: true,
                ready: imagesReady(el),
                meta: commentMeta(el, list[i]),
                rect,
                viewTop: rect.y - window.scrollY,
                viewHeight: window.innerHeight,
            };
        },
    };

    return true;
})();
