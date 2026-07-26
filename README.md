# yt-meta-archiver

Archives what a YouTube channel has published. One subcommand per kind of
thing:

```
yt-meta-archiver posts <channel> [output_dir] [max_posts] [--details] [--mt [workers]]
```

`posts` screenshots every community post on a channel, in YouTube's dark theme,
and writes a `posts.json` index beside the images. `--details` additionally
looks up each post's exact publication date and text from the post's own page.

`-v` (repeatable) applies to any subcommand.

## Layout

- `src/cli.rs` — the command line and its dispatch
- `src/posts/` — the community-posts extractor
- `src/browser.rs`, `src/http.rs`, `src/time.rs` — shared by every extractor

## TODO

- livestream chat comments
