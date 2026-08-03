# yt-meta-archiver

Archives what a YouTube channel has published. One subcommand per kind of thing:

```
yt-meta-archiver posts <channel> [--max n] [--details] [--comments [--min-likes n] [--subcomments n]] [--compress [quality]] [--mt [workers]] [--out <dir>]
```

`posts` screenshots every community post on a channel, in YouTube's dark theme, and writes a `posts.json` index beside
the images. `--details` additionally looks up each post's exact publication date and text from the post's own page, and
`--comments` opens each post and screenshots its top-level comments into
`comments/<post>/`, recording them under the post in the index. Both cost a page load per post, so both are off by
default. `--subcomments n` additionally opens each of those comments and captures up to `n` of its replies, into a
folder named after the comment they answer and nested under it in the index.

```
yt-meta-archiver comments <video> [--min-likes n] [--max n] [--subcomments n] [--pic] [--compress [quality]] [--out <dir>]
```

`comments` does the same for the comments under a single video, writing
`<out>/<video id>/comments.json`. `<video>` is a watch URL, a `youtu.be`,
`/live` or `/shorts` link, or a bare video id — a short is an ordinary video behind a different player, and its comments
are read off the same watch page as any other's. Threads are left collapsed unless `--subcomments` asks for them.

`--min-likes` keeps only the comments with at least that many likes, which on a well-watched video is the difference
between a page of them and thousands, and
`--max` stops after that many have been kept — loading stops as soon as enough are on the page, so it bounds the work as
well as the output. `--subcomments n`
opens each kept comment and records up to `n` of its replies, nested under it in the index; every thread taken has to be
opened and waited on, so it is the slowest part of a run. `--pic` screenshots each kept comment — and each reply, into a
folder named after the comment it answers — and names the file in the index; without it the run is metadata only, though
the page is still opened in a browser, there being no other way to read the comments.

```
yt-meta-archiver livechat <stream> [--pic] [--compress [quality]] [--out <dir>]
```

`livechat` follows a livestream's chat while it is happening and appends every message to `<out>/<video id>/chat.jsonl`
as it arrives — one JSON object per line, flushed per message, so the transcript is complete and readable at any point.
`<stream>` is either the stream (watch URL, `youtu.be` link, `/live`
link, or bare video id) or the channel airing it (`@handle` or channel URL), in which case whatever that channel is live
with right now is used.

A stream that is **already over** is read from its chat replay instead, which YouTube keeps and serves whole. `livechat`
notices this for itself and switches — no flag, no browser, and the entire chat in about a request per three minutes of
stream, which in practice is a few hundred times faster than the stream took to happen (a 3½-hour stream, 3000 messages,
in nine seconds). Replay lines carry an extra `offset_ms`, how far into the stream the message was said, which a live
capture cannot know. Not every VOD has one: a channel that turns its chat off, or has it removed afterwards, leaves
nothing to read.

`--pic` additionally screenshots each message, in the same dark theme `posts`
uses, and names the file on the message's line. A message that scrolls out of the chat before it can be captured is
still recorded; only the picture is lost. It applies to a live chat only — a replay comes from the API rather than a
page, so there is nothing to point a camera at, and a replay run says so and archives the transcript alone.

Following a live chat has no total to work through, so `livechat` runs until the chat closes or you interrupt it,
restarting the browser by itself if it dies mid-stream. Either way the transcript is appended to and deduplicated by
message id, so re-running over a directory adds what is missing rather than starting again — which is how a stream
caught halfway through live can be completed from its replay once it ends.

Every extractor writes lossless PNG by default. `--compress` writes WebP instead, at the quality given (1-100, 80 for a
bare flag) — captures are taken at 2x, so the text stays clean while the files run roughly a third the size. On
`livechat` and `comments` it needs `--pic`, there being nothing to compress otherwise.

`-v` (repeatable) applies to any subcommand.

## Todo

