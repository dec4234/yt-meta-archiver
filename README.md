# yt-meta-archiver

Archives what a YouTube channel has published. One subcommand per kind of
thing:

```
yt-meta-archiver posts <channel> [output_dir] [max_posts] [--details] [--comments [--min-likes n]] [--compress [quality]] [--mt [workers]]
```

`posts` screenshots every community post on a channel, in YouTube's dark theme,
and writes a `posts.json` index beside the images. `--details` additionally
looks up each post's exact publication date and text from the post's own page,
and `--comments` opens each post and screenshots its top-level comments into
`comments/<post>/`, recording them under the post in the index. Both cost a page
load per post, so both are off by default.

```
yt-meta-archiver comments <video> [--min-likes n] [--max n] [--pic] [--compress [quality]] [--out <dir>]
```

`comments` does the same for the comments under a single video, writing
`<out>/<video id>/comments.json`. `<video>` is a watch URL, a `youtu.be` or
`/live` link, or a bare video id. Only top-level comments are read; a thread's
replies are left out.

`--min-likes` keeps only the comments with at least that many likes, which on a
well-watched video is the difference between a page of them and thousands, and
`--max` stops after that many have been kept — loading stops as soon as enough
are on the page, so it bounds the work as well as the output. `--pic`
screenshots each kept comment and names the file in the index; without it the
run is metadata only, though the page is still opened in a browser, there being
no other way to read the comments.

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

Every extractor writes lossless PNG by default. `--compress` writes WebP
instead, at the quality given (1-100, 80 for a bare flag) — captures are taken
at 2x, so the text stays clean while the files run roughly a third the size. On
`livechat` and `comments` it needs `--pic`, there being nothing to compress
otherwise.

`-v` (repeatable) applies to any subcommand.

## Layout

- `src/cli.rs` — the command line and its dispatch
- `src/posts/` — the community-posts extractor
- `src/livechat/` — the live chat extractor
- `src/comments/` — the video-comments extractor, plus the comment reading and
  the injected script the posts extractor shares with it
- `src/browser.rs`, `src/clip.rs`, `src/http.rs`, `src/time.rs`, `src/video.rs` —
  shared by every extractor
