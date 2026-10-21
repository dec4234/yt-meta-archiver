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
yt-meta-archiver posts <channel> [--max n] [--details] [--comments [--min-likes n] [--subcomments n]] [--compress [quality]] [--mt [workers]] [--grab-profile-picture [dir]] [--out <dir>]
```

Screenshots every community post and writes a `posts.json` index. `--details` adds each post's date and text.
`--comments` adds its comments, `--subcomments n` adds up to `n` replies to each. All three cost a page load per post,
so all three are off by default.

## Video comments

```
yt-meta-archiver comments <video> [--min-likes n] [--max n] [--subcomments n] [--pic] [--compress [quality]] [--grab-profile-picture [dir]] [--out <dir>]
```

Writes the comments of one video to `<out>/<video id>/comments.json`. `<video>` is a watch, `youtu.be`, `/live` or
`/shorts` link, or a bare video id.

- `--min-likes` keeps only comments with at least that many likes.
- `--max` stops after that many are kept, which also stops the loading.
- `--subcomments n` adds up to `n` replies to each kept comment. This is the slowest part of a run.
- `--pic` screenshots each kept comment. Without it, the run is metadata only.

## Live chat

```
yt-meta-archiver livechat <stream> [--pic] [--compress [quality]] [--grab-profile-picture [dir]] [--grab-emoji [dir]] [--out <dir>]
```

Follows a livestream's chat and appends each message to `<out>/<video id>/chat.jsonl`, one JSON object per line.
`<stream>` is either the stream (watch, `youtu.be`, `/live` link, or video id) or the channel airing it (`@handle` or
channel URL).

A stream that is already over is read from its chat replay instead. `livechat` finds this for itself and changes over:
no flag, no browser, and much faster. Replay lines carry an `offset_ms`, how far into the stream the message was said.
Not every VOD keeps a replay.

`--pic` screenshots each message. It applies to a live chat only, because a replay comes from the API and not a page.

A standard emoji is written in `text` as its Unicode character, for example `😂`. A custom emoji (a channel member emoji,
or a YouTube emoji such as `:yt:` or `:hand-pink-waving:`) is an image, so `text` keeps its shortcut, for example
`:_channelEmote:`. The line also gets an `emojis` list with the `id`, `shortcut` and image `url` of each custom emoji.
`--grab-emoji` saves these images into one shared directory (`emojis` for a bare flag) and adds an `image` path to each.
An image that is already there is used again, not downloaded again.

The run continues until the chat closes or you stop it, and restarts the browser by itself if it dies. The transcript is
appended to and deduplicated by message id, so you can re-run over the same directory to add what is missing.

## Profile pictures

```
yt-meta-archiver pfp <channel>... [--from <file>] [--size pixels] [--refresh] [--mt [workers]] [--out <dir>]
```

Saves the profile picture of each channel to `<out>` (`profile-pictures` by default). This is the same shared directory
that `--grab-profile-picture` uses, so the two use the pictures of each other. `<channel>` is a handle (`@name`), a
channel id (`UC…`), or a channel URL. `pfp` does not need a browser.

- `--from` also reads channels from a file, one on each line. The tool skips blank lines and lines that start with `#`.
- `--size` sets the size of the square picture in pixels (16-900, 176 by default).
- `--refresh` downloads each picture again, also when it is already in the directory.

The tool writes one line to stdout for each channel: the input and the saved path, with a tab between them. If one or
more channels have no picture, the exit code is not zero.

## Channel banners

```
yt-meta-archiver banner <channel>... [--from <file>] [--cropped] [--refresh] [--mt [workers]] [--out <dir>]
```

Saves the banner (header image) of each channel to `<out>` (`banners` by default). The file names are the same as in
`pfp`. `banner` takes the same channel input, `--from`, `--refresh` and `--mt` as `pfp`, and gives the same stdout lines
and exit code. It does not need a browser.

By default, the tool keeps the original upload with no crop. `--cropped` keeps the wide strip that a desktop browser
shows.

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
- `--grab-profile-picture` saves the profile picture of each author into one shared directory (`profile-pictures` for
  a bare flag). Each entry gets a `profile_picture` path. All subcommands and all runs use the same directory, and a
  picture that is already there is used again, not downloaded again. On `posts` it saves the channel picture, plus each
  commenter's picture with `--comments`.
- `-v` (repeatable) increases the log detail. It applies to any subcommand.