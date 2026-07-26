// Injected into YouTube's popout live chat. Everything the Rust side needs
// from the DOM is exposed on `window.__ytc` and returns JSON-serialisable
// values.
(() => {
    const PAD = 8; // px of page background left around each captured message

    // Message ids we have already handed to the Rust side. Chat runs for hours,
    // but ids are short and YouTube trims the DOM, so this stays small enough.
    //
    // Kept on `window`, not in this closure: the Rust side re-injects this
    // script whenever it finds `__ytc` missing, and a fresh Set would make
    // every message still on the page look new all over again.
    const seen = window.__ytcSeen || (window.__ytcSeen = new Set());

    // The renderers we archive, and what we call each kind. Anything not listed
    // here — engagement nudges, poll banners, the "welcome to live chat"
    // notice — is chrome rather than something a viewer said.
    const KINDS = {
        "YT-LIVE-CHAT-TEXT-MESSAGE-RENDERER": "text",
        "YT-LIVE-CHAT-PAID-MESSAGE-RENDERER": "superchat",
        "YT-LIVE-CHAT-PAID-STICKER-RENDERER": "supersticker",
        "YT-LIVE-CHAT-MEMBERSHIP-ITEM-RENDERER": "membership",
        "YT-LIVE-CHAT-SPONSORSHIPS-GIFT-PURCHASE-ANNOUNCEMENT-RENDERER": "gift",
        "YT-LIVE-CHAT-SPONSORSHIPS-GIFT-REDEMPTION-ANNOUNCEMENT-RENDERER": "gift-redemption",
    };

    // YouTube's own CSS keys off `html[dark]`, so setting the attribute forces
    // the dark palette regardless of what the appearance setting resolved to.
    function forceDarkTheme() {
        document.documentElement.setAttribute("dark", "");
        for (const el of document.querySelectorAll("yt-live-chat-app, yt-live-chat-renderer")) {
            el.setAttribute("is-dark-theme", "");
            el.setAttribute("dark", "");
        }
    }

    // Popups that would otherwise be painted over a clipped message.
    function hideOverlays() {
        const selectors = [
            "tp-yt-iron-overlay-backdrop",
            "ytd-consent-bump-v2-lightbox",
            "yt-live-chat-restricted-participation-renderer",
            "#dialog",
        ];
        for (const sel of selectors) {
            for (const el of document.querySelectorAll(sel)) {
                el.style.setProperty("display", "none", "important");
            }
        }
    }

    function scroller() {
        return document.querySelector("yt-live-chat-item-list-renderer #item-scroller");
    }

    // The message list, oldest first. `#items` is the live region YouTube
    // appends each new message to.
    function items() {
        const list = document.querySelector("yt-live-chat-item-list-renderer #items");
        return list ? [...list.children].filter((el) => KINDS[el.tagName]) : [];
    }

    // Two things a viewer can see but the chat markup does not carry: who the
    // author is as a channel, and when the message was actually sent. Both are
    // in the `get_live_chat` responses that drive the chat, so we watch those
    // go past and keep what the DOM drops, keyed by message id. Survives
    // re-injection for the same reason `seen` does.
    const meta = window.__ytcMeta || (window.__ytcMeta = new Map());

    // Every renderer in a continuation response carries its own id alongside
    // the fields we are after, so rather than walking YouTube's particular
    // nesting — which changes — we look for that pairing anywhere in the tree.
    function harvest(node, depth) {
        if (!node || typeof node !== "object" || depth > 12) return;
        if (Array.isArray(node)) {
            for (const v of node) harvest(v, depth + 1);
            return;
        }
        if (typeof node.id === "string" && typeof node.timestampUsec === "string") {
            stats.harvested++;
            meta.set(node.id, {
                timestamp_usec: node.timestampUsec,
                author_channel_id: node.authorExternalChannelId || null,
            });
        }
        for (const k in node) harvest(node[k], depth + 1);
    }

    // Counters for how the hook is faring, reported by `state` so a -vv run can
    // tell "chat is quiet" apart from "we stopped seeing the responses".
    const stats = window.__ytcStats || (window.__ytcStats = {fetches: 0, chat_fetches: 0, chat_xhrs: 0, harvested: 0});

    function isChatResponse(url) {
        return typeof url === "string" && url.includes("get_live_chat");
    }

    // Note: the backlog a document starts with is not reachable this way — it
    // is rendered from the page's own `ytInitialData`, which carries no
    // timestamps to find. That costs us nothing, because the backlog is
    // precisely what `prime` excludes from the transcript.

    // Chat is delivered over both fetch and XHR depending on the build, so both
    // are wrapped. Responses are only ever read from a clone — the page must
    // still get its own body, untouched, or chat stops rendering.
    function hookChatResponses() {
        if (window.__ytcHooked) return;
        window.__ytcHooked = true;

        const fetch_ = window.fetch;
        window.fetch = function (...args) {
            const result = fetch_.apply(this, args);
            try {
                stats.fetches++;
                const url = typeof args[0] === "string" ? args[0] : args[0] && args[0].url;
                if (isChatResponse(url)) {
                    stats.chat_fetches++;
                    result.then((res) => res.clone().json().then((j) => harvest(j, 0))).catch(() => {});
                }
            } catch (e) {
                /* never let our bookkeeping break the page's own request */
            }
            return result;
        };

        const open_ = XMLHttpRequest.prototype.open;
        XMLHttpRequest.prototype.open = function (method, url, ...rest) {
            this.__ytcChat = isChatResponse(url);
            if (this.__ytcChat) {
                stats.chat_xhrs++;
                this.addEventListener("load", () => {
                    try {
                        harvest(JSON.parse(this.responseText), 0);
                    } catch (e) {
                        /* not JSON, or not ours */
                    }
                });
            }
            return open_.call(this, method, url, ...rest);
        };
    }

    // A message body, flattened to text. Emoji are images, and the tooltip is
    // the `:shortcode:` a viewer would have typed — `alt` gives the same name
    // without the colons, which would run into the surrounding words.
    function runsText(node) {
        if (!node) return null;
        let out = "";
        for (const child of node.childNodes) {
            if (child.nodeType === Node.TEXT_NODE) {
                out += child.textContent;
            } else if (child.tagName === "IMG") {
                const shortcode = child.getAttribute("shared-tooltip-text");
                out += shortcode || (child.alt ? `:${child.alt}:` : "");
            } else {
                out += child.textContent || "";
            }
        }
        return out.trim() || null;
    }

    function text(el, selector) {
        const node = el.querySelector(selector);
        return node ? node.textContent.trim() || null : null;
    }

    // "Member (6 months)", "Moderator", "Verified" — the badges beside a name.
    function badges(el) {
        return [...el.querySelectorAll("#author-badges yt-live-chat-author-badge-renderer, #chat-badges yt-live-chat-author-badge-renderer")]
            .map((b) => b.getAttribute("aria-label") || b.getAttribute("shared-tooltip-text") || b.getAttribute("type"))
            .filter(Boolean);
    }

    function describe(el) {
        const data = meta.get(el.id) || {};
        return {
            id: el.id || null,
            kind: KINDS[el.tagName] || "other",
            author: text(el, "#author-name"),
            author_channel_id: data.author_channel_id || null,
            badges: badges(el),
            // Absent on a sticker or a bare membership announcement, which say
            // everything they have to say in `kind` and `amount`.
            text: runsText(el.querySelector("#message")),
            // Superchats and stickers only.
            amount: text(el, "#purchase-amount") || text(el, "#purchase-amount-chip"),
            // The membership tier, or the gift announcement's own wording.
            header: text(el, "#header-subtext") || text(el, "#primary-text"),
            // Microseconds since the epoch, straight from YouTube, which beats
            // our own arrival time by however long the message took to reach us.
            timestamp_usec: data.timestamp_usec || null,
        };
    }

    // The band of the window where chat is actually painted: the message list's
    // own scroll container, clamped to the window.
    function paintedBand() {
        const box = scroller();
        const b = box ? box.getBoundingClientRect() : null;
        return {
            top: b ? Math.max(0, b.top) : 0,
            bottom: b ? Math.min(window.innerHeight, b.bottom) : window.innerHeight,
        };
    }

    // Viewport coordinates of a message, padded. The chat page itself never
    // scrolls — the message list is its own scroll container — so these are
    // already the page coordinates a capture clip wants.
    //
    // The padding is a nicety and gets clipped rather than insisted upon. Above
    // and below it may only take what the gap to the neighbouring message
    // offers: chat lines are stacked all but flush, so a full PAD upwards
    // reaches into whoever spoke last and captures the tail of their text or
    // the bottom of their avatar. Sideways there is no such neighbour, only the
    // list's own margin, so the full padding stands.
    function paddedRect(el) {
        const r = el.getBoundingClientRect();
        const band = paintedBand();
        // A neighbour that is not painted at all — a renderer we do not archive,
        // or one YouTube has hidden — reports an empty box at the origin, which
        // must not be mistaken for an edge to keep clear of.
        const edge = (sib) => {
            const b = sib ? sib.getBoundingClientRect() : null;
            return b && b.height > 0 ? b : null;
        };
        const prev = edge(el.previousElementSibling);
        const next = edge(el.nextElementSibling);
        const ceiling = Math.max(band.top, prev ? prev.bottom : band.top);
        const floor = Math.min(band.bottom, next ? next.top : band.bottom);

        const x = Math.max(0, r.left - PAD);
        // Rounded inwards: the clip is taken at 2x, and a fractional edge
        // smears the neighbouring row back into the picture we just excluded.
        const y = Math.ceil(Math.max(ceiling, r.top - PAD));
        const bottom = Math.floor(Math.min(floor, r.bottom + PAD));
        return {
            x,
            y,
            width: Math.min(r.width + PAD * 2, document.documentElement.clientWidth - x),
            height: Math.max(0, bottom - y),
        };
    }

    // Whether a message is wholly painted. Its own box has to be within the
    // band — a clip reaching past that would capture background — but the
    // padding around it does not, or the newest message, which is the one we
    // most want, could never qualify.
    function onScreen(el) {
        const r = el.getBoundingClientRect();
        if (r.height <= 0 || r.width <= 0) return false;
        const band = paintedBand();
        return r.top >= band.top && r.bottom <= band.bottom;
    }

    // Installed the moment this script runs rather than from `prepare`: the
    // Rust side arranges for it to run before the page's own bundle, and the
    // hook is only any use if it is in place before YouTube takes its own
    // reference to `fetch`. Touching nothing but `window`, it is safe this
    // early, when there is not yet a document to speak of.
    hookChatResponses();

    window.__ytc = {
        prepare() {
            hookChatResponses();
            forceDarkTheme();
            hideOverlays();
            return {ready: !!document.querySelector("yt-live-chat-item-list-renderer #items")};
        },

        // Marks everything already on the page as seen, so a run archives the
        // stream from the moment it starts watching rather than replaying
        // whatever backlog YouTube happened to render.
        //
        // Returns the ids, not just a count: this page reloads itself through a
        // long stream, and each reload renders that same backlog afresh, so the
        // caller has to keep hold of them to go on ignoring them.
        prime() {
            const ids = [];
            for (const el of items()) {
                if (el.id) {
                    seen.add(el.id);
                    ids.push(el.id);
                }
            }
            return ids;
        },

        // Every message that has appeared since the last call, oldest first,
        // capped at `max` so one slow round cannot hand back an unbounded
        // batch. Anything beyond the cap is picked up by the next call.
        poll(max) {
            forceDarkTheme();
            hideOverlays();
            const out = [];
            for (const el of items()) {
                if (!el.id || seen.has(el.id)) continue;
                seen.add(el.id);
                out.push(describe(el));
                if (out.length >= max) break;
            }
            return out;
        },

        // Where message `id` is right now, for the capture clip. Chat keeps
        // scrolling underneath us, so this is asked immediately before the
        // screenshot rather than reused from `poll`.
        locate(id) {
            const el = items().find((e) => e.id === id);
            if (!el) return {exists: false};
            hideOverlays();
            // A busy chat scrolls a message out of view between our reading it
            // and our capturing it, so it is scrolled back to rather than
            // written off — it is still in the list long after it leaves the
            // screen. `resume` puts the chat back afterwards.
            if (!onScreen(el)) {
                el.scrollIntoView({block: "center", behavior: "instant"});
            }
            return {exists: true, onScreen: onScreen(el), rect: paddedRect(el)};
        },

        // Returns the list to the bottom, where YouTube takes its own
        // auto-scrolling back over and stops queuing new messages. Called once
        // a batch of captures is done.
        resume() {
            const box = scroller();
            if (box) box.scrollTop = box.scrollHeight;
            return true;
        },

        // Whether the chat is still running. YouTube swaps the composer for a
        // notice once a stream ends, and drops the item list entirely if chat
        // is turned off.
        state() {
            return {
                items: items().length,
                live: !!document.querySelector("yt-live-chat-item-list-renderer #items"),
                hooked: !!window.__ytcHooked,
                known: meta.size,
                stats,
            };
        },
    };

    return true;
})();
