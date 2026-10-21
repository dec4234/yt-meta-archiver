# livechat-analyze

Reads a `chat.jsonl` transcript from `yt-meta-archiver livechat` and shows who talked, how much, and when.

```
livechat-analyze <path> [--top n] [--bucket spec] [--author who] [--emoji n] [--width n] [--plain] [--snapshot COLSxROWS]
```

`<path>` is the `chat.jsonl` file or the directory that holds it.

```
cargo run -p livechat-analyze -- livechat/youtube_id
```

## The dashboard

In a terminal you get a dashboard: a chart of the message volume, the ranking of the chatters, the counts, and a bar for
the time bucket at the cursor. It opens at the busiest moment of the stream.

- `←`/`→` (or `h`/`l`) move the cursor. `Home`/`End` go to the ends. `p` goes back to the peak.
- `+`/`-` make the time buckets wider or narrower. The cursor keeps its moment, and the file is not read again.
- `↑`/`↓` (or `k`/`j`) and `PgUp`/`PgDn` move through the ranking.
- `f` or `Enter` shows or hides the selected chatter's messages on the chart.
- `q` or `Esc` stops the program.

## The plain report

`--plain` prints the same numbers as flat text. This is also the result when the output is not a terminal, so pipes and
redirections work without the flag.

```
livechat/youtube_id/chat.jsonl
  23,700 messages   1,467 authors   3h 56m span   (replay offsets)

Top chatters
    1  @chatter 1      415    1.8%
    2  @chatter 2      407    1.7%
    3  @chatter 3      383    1.6%
       … and 1,464 more

Activity (10m buckets, peak 2:10 with 1,484)
  0:00  ##########################        1,334
  0:10  ######################            1,097
  0:20  ########################          1,220
  …
Badges       Moderator 314   Owner 1   Verified 65
Messages     mean 33 chars   median 17   longest 725
Emoji        :face-red-droopy-eyes: 8,022   :face-green-smiling: 736   :mushroom: 615
```

A section is left out when the transcript has no data for it.

`--snapshot 120x40` draws one dashboard frame at that size and prints it as text. Use it where there is no terminal.

## One chatter

`--author` selects one person by handle, by channel id, or by a part of either. An exact handle or id always wins, so
`@bot` does not select `@bottom`. A part of a name is sufficient while it selects one person only; if it selects more,
the program says which and stops.

```
livechat-analyze livechat/cHu_UCriJ-Q --author @chatter 4
```

This adds their badges, their money, their first and last message, and their messages across the stream. Their bars use
the same time buckets as the chart above, so you can compare the two bar for bar. The bars are scaled to that person's
own busiest bucket, because one person against twenty thousand messages is otherwise flat at zero. The share column
gives the true value: how much of each bucket was theirs.

On the dashboard, `--author` opens on that person with their overlay drawn.

## Flags

- `--top n` — how many chatters to rank. 20 by default.
- `--bucket spec` — the time bucket for the chart: `30s`, `5m`, `1h`, or `auto`. `auto` is the default and gives the
  stream approximately forty bars.
- `--author who` — select one chatter, as above.
- `--emoji n` — how many emoji to list. 10 by default. It counts custom emoji by their shortcode and standard emoji by
  their character. It also counts the shortcodes of standard emoji in transcripts from older versions.
- `--width n` — the bar width for the plain report, in columns. 50 by default. The dashboard sizes its own chart to the
  terminal.
- `--plain` — print the flat text report instead of the dashboard.
- `--snapshot COLSxROWS` — draw one dashboard frame at that size and print it as text.

## Time

A replay gives the offset of each message into the stream, and the report says `replay offsets`.

A live capture has no such offset — it was watching a page, not a timeline — so the time is measured from the first
message that was caught, at the 1 second resolution of `sent_utc`. The report says so, because that zero point is not
the start of the stream. If some lines of a file have offsets, all of the file is read as a replay: the two clocks start
at different moments, and a mix of them would put messages in the wrong buckets.

A transcript with no timestamps keeps all of its counts. Only the chart goes away.

## Bad lines

A line that cannot be parsed is counted and skipped. The archiver appends while the stream is live and can stop in the
middle of a write, so a cut last line is usual. The report gives the number of skipped lines. Amounts in a shape that
the program cannot read are counted in the same way, so a currency that is handled incorrectly is visible.

## Tests

```
cargo test -p livechat-analyze
```