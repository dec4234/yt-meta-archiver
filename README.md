# yt-meta-archiver

Archive tool for community posts, and comments on videos, community posts, and shorts. Also archive live chat from
ongoing livestreams and VODs.

A lot of this was AI generated, use at your own risk. You may be subject to rate limiting or restrictions from YouTube
if you hit the site too much.

## Setup

`posts`, `comments` and a live `livechat` use a real browser, so you must install one before you run them. Install
Google Chrome, Chromium or Microsoft Edge. Edge is sufficient, because it speaks the same DevTools protocol. Only a
`livechat` replay and `livechat-analyze` do not need a browser.

The tool starts the browser itself in headless mode. Do not start a browser first, and do not install a driver such as
chromedriver.

The tool looks for the browser in this sequence:

1. The path in the `CHROME_PATH` environment variable.
2. Chrome in `%ProgramFiles%`, `%ProgramFiles(x86)%` or `%LOCALAPPDATA%` (Windows).
3. Edge in `%ProgramFiles%` or `%ProgramFiles(x86)%` (Windows).
4. `/usr/bin/google-chrome` or `/usr/bin/chromium` (Linux).
5. `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome` (macOS).

If your browser is in a different location, or if the run stops with `no Chrome/Chromium/Edge binary found`, set
`CHROME_PATH` to the browser program:

```
# Windows (PowerShell)
$env:CHROME_PATH = "C:\Program Files\Google\Chrome\Application\chrome.exe"

# Linux / macOS
export CHROME_PATH=/usr/bin/chromium
```

The browser runs with a new, empty profile on each run. Thus you are not signed in to YouTube, and you get only the
public data. Each run also opens its own browser: if you run more than one command at the same time, each command uses
more memory and more CPU.

## Community posts

```
yt-meta-archiver posts <channel> [--max n] [--details] [--comments [--min-likes n] [--subcomments n]] [--compress [quality]] [--mt [workers]] [--out <dir>]
```

Screenshots every community post and writes a `posts.json` index. `--details` adds each post's date and text.
`--comments` adds its comments, `--subcomments n` adds up to `n` replies to each. All three cost a page load per post,
so all three are off by default.

## Video comments

```
yt-meta-archiver comments <video> [--min-likes n] [--max n] [--subcomments n] [--pic] [--compress [quality]] [--out <dir>]
```

Writes the comments of one video to `<out>/<video id>/comments.json`. `<video>` is a watch, `youtu.be`, `/live` or
`/shorts` link, or a bare video id.

- `--min-likes` keeps only comments with at least that many likes.
- `--max` stops after that many are kept, which also stops the loading.
- `--subcomments n` adds up to `n` replies to each kept comment. This is the slowest part of a run.
- `--pic` screenshots each kept comment. Without it, the run is metadata only.

## Live chat

```
yt-meta-archiver livechat <stream> [--pic] [--compress [quality]] [--out <dir>]
```

Follows a livestream's chat and appends each message to `<out>/<video id>/chat.jsonl`, one JSON object per line.
`<stream>` is either the stream (watch, `youtu.be`, `/live` link, or video id) or the channel airing it (`@handle` or
channel URL).

A stream that is already over is read from its chat replay instead. `livechat` finds this for itself and changes over:
no flag, no browser, and much faster. Replay lines carry an `offset_ms`, how far into the stream the message was said.
Not every VOD keeps a replay.

`--pic` screenshots each message. It applies to a live chat only, because a replay comes from the API and not a page.

The run continues until the chat closes or you stop it, and restarts the browser by itself if it dies. The transcript is
appended to and deduplicated by message id, so you can re-run over the same directory to add what is missing.

## Reading a chat back

```
livechat-analyze <path> [--top n] [--bucket spec] [--author who] [--plain]
```

`livechat-analyze` is a second binary in this workspace. It reads a `chat.jsonl` transcript back and says who talked,
how much, and when across the stream on a terminal, or as flat text through a pipe. See
[livechat-analyze/README.md](livechat-analyze/README.md).

## Other flags

- `--compress` writes WebP instead of the default lossless PNG, at the quality given (1-100, 80 for a bare flag). On
  `livechat` and `comments` it needs `--pic`.
- `-v` (repeatable) increases the log detail. It applies to any subcommand.