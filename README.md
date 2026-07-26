# yt-meta-archiver

Archives what a YouTube channel has published. One subcommand per kind of
thing:

```
yt-meta-archiver posts <channel> [output_dir] [max_posts] [--details] [--compress [quality]] [--mt [workers]]
```

`posts` screenshots every community post on a channel, in YouTube's dark theme,
and writes a `posts.json` index beside the images. `--details` additionally
looks up each post's exact publication date and text from the post's own page.

```
yt-meta-archiver livechat <stream> [--pic] [--compress [quality]] [--out <dir>]
```

`livechat` follows a livestream's chat while it is happening and appends every
message to `<out>/<video id>/chat.jsonl` as it arrives — one JSON object per
line, flushed per message, so the transcript is complete and readable at any
point. `<stream>` is either the stream (watch URL, `youtu.be` link, `/live`
link, or bare video id) or the channel airing it (`@handle` or channel URL), in
which case whatever that channel is live with right now is used.

`--pic` additionally screenshots each message, in the same dark theme `posts`
uses, and names the file on the message's line. A message that scrolls out of
the chat before it can be captured is still recorded; only the picture is lost.

Unlike `posts` there is no total to work through, so `livechat` runs until the
chat closes or you interrupt it, restarting the browser by itself if it dies
mid-stream.

Both extractors write lossless PNG by default. `--compress` writes WebP instead,
at the quality given (1-100, 80 for a bare flag) — captures are taken at 2x, so
the text stays clean while the files run roughly a third the size. On `livechat`
it needs `--pic`, there being nothing to compress otherwise.

`-v` (repeatable) applies to any subcommand.

## Layout

- `src/cli.rs` — the command line and its dispatch
- `src/posts/` — the community-posts extractor
- `src/livechat/` — the live chat extractor
- `src/browser.rs`, `src/http.rs`, `src/time.rs` — shared by every extractor
