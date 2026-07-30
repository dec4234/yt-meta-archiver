// Injected into a page whose comment section we want — a single community
// post, or a video's watch page. The comment markup is the same on both, so
// one script serves them; only the chrome to get out of the way differs.
//
// Exposes the same `window.__yta` surface as the feed archiver —
// `prepare`/`count`/`focus`/`align` — plus `metas()`, which reads every
// top-level comment at once so the caller can decide what is worth capturing
// before paying for any screenshots, and `likeCounts()`, the cheap slice of
// that used to decide when enough has been loaded.
//
// Replies are left collapsed unless the caller asks for them with
// `subcomments(true)`, which folds each thread's loaded replies into the list
// `focus`/`align` address, immediately after the comment they hang off. Such a
// caller names what it wants with `select` rather than by position, since
// replies arriving above it move a comment along mid-capture.
(() => {
    const PAD = 12; // px of page background left around each captured comment

    // Comments are parked this far down the viewport rather than flush with
    // the top, so YouTube's sticky masthead is never painted over them.
    const TOP_MARGIN = 220;

    // Whether replies count as capturable elements. Off by default, so a
    // caller that only wants top-level comments addresses them by their
    // position in the thread list and nothing an expanded thread does can
    // shift what an index means.
    let withReplies = false;

    // The element the next capture is aimed at, pinned by `select`. A capture
    // is several round trips — focus, resize, align — and with replies in the
    // list a position is only true for as long as nothing above it changes:
    // one reply arriving in an earlier thread, or a collapsing expander,
    // slides every later comment along, and re-deriving the target from its
    // index part way through captures a neighbour, or the empty gap one left
    // behind. Holding the element itself makes the page moving under us
    // harmless.
    let picked = null;

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
    //
    // Expanding a thread wraps each of its replies in a thread renderer of its
    // own, inside the parent's reply list, so the nested ones are dropped:
    // counting them would have the list grow by six the moment a six-reply
    // thread is opened, and every index past it — which is how a comment is
    // addressed, and what its screenshot is named after — would mean a
    // different comment than it did a moment earlier.
    function threads() {
        return [...document.querySelectorAll("ytd-comment-thread-renderer")].filter((el) => !el.closest("#replies"));
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

    // A thread's replies, in the order they are shown, and only those actually
    // loaded — a collapsed thread has none of them in the markup at all. The
    // two selectors are the current and the older reply element; a page uses
    // one or the other, never both, so the first non-empty answer is it.
    function replies(thread) {
        if (!withReplies) return [];
        let list = [...thread.querySelectorAll("#replies ytd-comment-view-model")];
        if (list.length === 0) {
            list = [...thread.querySelectorAll("#replies ytd-comment-renderer")];
        }
        return list.filter(visible);
    }

    // Everything capturable, in page order: each top-level comment followed by
    // whichever of its replies have been loaded. With `subcomments` off this is
    // exactly the top-level comments, so an index means the same thing to a
    // caller that never asked for replies.
    function nodes() {
        const out = [];
        for (const thread of threads()) {
            out.push({el: comment(thread), thread});
            // A reply has no reply count of its own to read, so it is its own
            // metadata root.
            for (const reply of replies(thread)) out.push({el: reply, thread: null});
        }
        return out;
    }

    // What `focus` and `align` are addressing: whatever `select` pinned, or —
    // for a caller that never pins anything, which is every caller that only
    // wants top-level comments — the `i`th capturable element.
    function node(i) {
        if (picked) return picked.el.isConnected ? picked : null;
        const list = nodes();
        return i < list.length ? list[i] : null;
    }

    // Threads whose "N replies" toggle we have already clicked. Clicking it a
    // second time collapses the thread — which loses the replies we came for,
    // and leaves YouTube re-rendering the section around it, so neighbouring
    // threads stop being laid out at all and capture as nothing. Remembering
    // what we opened means a toggle is never clicked twice however the markup
    // reads at the time.
    const opened = new WeakSet();

    // Whether a thread's replies have been opened. The "N replies" toggle stays
    // in the markup once it has been used and is marked hidden, YouTube putting
    // its own collapse control in its place — the attribute is the signal, not
    // the button's absence. Counting loaded replies is only the fallback for a
    // thread with no toggle at all, since it reads as shut for the moment
    // between the click and the first reply arriving.
    function threadOpen(thread) {
        if (opened.has(thread)) return true;
        const toggle = replyToggle(thread);
        if (toggle) return toggle.hasAttribute("hidden");
        return thread.querySelectorAll("#replies ytd-comment-view-model, #replies ytd-comment-renderer").length > 0;
    }

    // The "N replies" button that opens a collapsed thread.
    //
    // Deliberately not filtered by visibility: YouTube keeps the reply section
    // of an untouched thread marked hidden, and only un-hides it a frame or so
    // after the thread is scrolled to, which would have us skip the very
    // threads we came for. A click lands on the handler either way.
    function replyToggle(thread) {
        return thread.querySelector("#replies #more-replies");
    }

    // The element a click has to land on: `querySelector` answers in document
    // order, so asking for the button and its wrapper together always returns
    // the wrapper, and YouTube wires the handler to the button inside it.
    function clickable(el) {
        return el.querySelector("button, tp-yt-paper-button, a") || el;
    }

    // The control that loads a thread's next page of replies, present only
    // while it has one left to give.
    function replyContinuations(thread) {
        return [...thread.querySelectorAll("#replies ytd-continuation-item-renderer")];
    }

    function visible(el) {
        return el.offsetParent !== null && el.getBoundingClientRect().height > 0;
    }

    // An element occupying no space at all. Its padded rect is still a valid
    // clip, so a capture of it succeeds and writes out a square of empty page
    // background; the caller is told so it can skip it instead.
    function collapsed(el) {
        const r = el.getBoundingClientRect();
        return r.width < 1 || r.height < 1;
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

        // Whether replies are capturable elements too. Turning this on changes
        // what `focus`/`align`/`at` index, so it is set once, before anything
        // is captured.
        subcomments(on) {
            withReplies = !!on;
            return withReplies;
        },

        // Opens thread `i`'s replies and asks for another page of them,
        // reporting how many are loaded and whether there is more to come.
        // Loading is a fetch, so the caller polls this rather than expecting
        // one call to settle it.
        showReplies(i) {
            const list = threads();
            if (i >= list.length) return {count: 0, more: false};
            const thread = list[i];
            hideOverlays();
            // The continuation only fetches once it is on screen, so bring the
            // thread into view before asking.
            thread.scrollIntoView({block: "center", behavior: "instant"});

            let clicked = 0;
            if (!threadOpen(thread)) {
                const toggle = replyToggle(thread);
                if (toggle) {
                    // Marked open before the click rather than after, so a
                    // second call can never mistake a thread mid-expansion for
                    // one still shut and click it closed again.
                    opened.add(thread);
                    clickable(toggle).click();
                    clicked++;
                }
            } else {
                for (const item of replyContinuations(thread)) {
                    clickable(item).click();
                    clicked++;
                }
            }
            // Whatever was just clicked has not arrived yet, so "more" means
            // there was something to click at all: the caller waits and asks
            // again, and stops once the count settles.
            return {count: replies(thread).length, more: clicked > 0};
        },

        // Thread `i`'s replies, in the order they are shown.
        replyMetas(i) {
            const list = threads();
            if (i >= list.length) return [];
            return replies(list[i]).map((reply) => commentMeta(reply, null));
        },

        // Aims the next capture at thread `i`'s comment (`r` below zero) or at
        // its `r`th reply, and reports where it sits in the list — which is
        // what the caller logs it as, not how `focus` and `align` find it
        // again. -1 for something the page no longer has, which also unpins
        // whatever was aimed at before.
        select(i, r) {
            picked = null;
            const list = threads();
            if (i >= list.length) return -1;
            let base = 0;
            for (let k = 0; k < i; k++) base += 1 + replies(list[k]).length;
            if (r < 0) {
                picked = {el: comment(list[i]), thread: list[i]};
                return base;
            }
            const under = replies(list[i]);
            if (r >= under.length) return -1;
            // A reply has no reply count of its own to read, so it is its own
            // metadata root.
            picked = {el: under[r], thread: null};
            return base + 1 + r;
        },

        // Brings comment `i` into view and reports whether it can be captured.
        focus(i) {
            const found = node(i);
            if (!found) return {exists: false};
            const {el, thread} = found;
            el.scrollIntoView({block: "center", behavior: "instant"});
            hideOverlays();
            expand(el);
            return {
                exists: true,
                ready: imagesReady(el),
                empty: collapsed(el),
                meta: commentMeta(el, thread),
                rect: paddedRect(el),
            };
        },

        // Parks comment `i` just below the sticky masthead and re-measures it.
        // Called once the viewport has been sized to fit, immediately before
        // the capture, so the clip matches what is actually painted.
        align(i) {
            const found = node(i);
            if (!found) return {exists: false};
            const {el, thread} = found;
            hideOverlays();
            const top = el.getBoundingClientRect().top + window.scrollY;
            window.scrollTo(0, Math.max(0, top - TOP_MARGIN));

            const rect = paddedRect(el);
            return {
                exists: true,
                ready: imagesReady(el),
                empty: collapsed(el),
                meta: commentMeta(el, thread),
                rect,
                viewTop: rect.y - window.scrollY,
                viewHeight: window.innerHeight,
            };
        },
    };

    return true;
})();
