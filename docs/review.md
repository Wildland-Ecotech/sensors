# Reviewing in a browser

```bash
sensors review
sensors review --port 8080 --db /path/to/other.sqlite
```

`sensors review` serves the review pages at <http://127.0.0.1:7172/> until
you press Ctrl+C. The port differs from `wot review`'s 7171, so both can run
at once. It only listens on 127.0.0.1, so only this computer can open it,
and it has no login. Pages are plain HTML, in your system's light or dark
mode, and work without JavaScript. The only script adds hover tooltips to
the charts.

## Pages

- **Overview:** how many log files and lines were imported and when, and
  each sensor's measures with their latest reading, when it was taken, how
  many readings there are, and since when. Times are as the logs show them,
  see [Time zones](readings.md#time-zones). **Analyze now** reads new lines.
- **Sensors:** a chart of each measure, such as temperature and humidity,
  with a line for each sensor, see [Graphs](#graphs). Links above the charts
  show the last day, week, month or year up to the newest reading, or all of
  it. Under each chart, each sensor's latest reading, and its lowest, mean
  and highest over the range. Below them, CSV downloads of the range.
- **History:** the lines said in channels one day at a time, the way irssi
  shows them, newest first, 200 to a page. It opens on the most recent day,
  with filters for a channel, a day and text the lines contain. Below them,
  every day with lines, by month, links to that day. Private messages are
  left out.
- **Analyze:** the channels to process, the log folders the imported files
  came from, and **Analyze now**, which does what `sensors analyze` does
  with them. The channels are changed with `sensors analyze --channel`, see
  [Channels](logs.md#channels). Tick
  **Read the readings from every line again** to do what `--rebuild` does.
  It runs in the background, and the page reloads itself with a progress bar
  for each step until it is done. Other pages keep working meanwhile,
  showing the data from before it, with a banner. New network folders inside
  the log folders are found too, but a log folder never imported needs
  `sensors analyze FOLDER` once.

## Graphs

- **One device.** Every nick that posted to a channel counts as the same
  device, since one that reconnects often comes back as `greenhouse1` or
  `greenhouse10`. So a line is one measure from one sensor, every nick
  together.
- **Points.** A range is cut into points of one size, the smallest that
  keeps a line under 800 points: 2 minutes for a day, 15 for a week, an hour
  for a month, 12 hours for a year, and a day or a week for everything. The
  line goes through each point's mean, and a faint band spans its lowest to
  highest reading.
- **Gaps.** Where a point has no readings, such as when the device was
  offline, the line breaks, so a gap shows as a gap.
- **Times.** Points and ticks fall on the logs' hours and midnights, see
  [Time zones](readings.md#time-zones).
- **Copies.** Points under an hour average the readings themselves, and a
  reading posted under two nicks at the same moment counts once. Longer
  points add up the hourly totals, which count it twice; such copies are
  rare.
- **Units.** `temp` is shown as °F, since the greenhouse's temperatures step
  by 0.18, a tenth of a degree Celsius, and `humid` as % RH. Other measures
  show their field's name.
- **Colors.** Each sensor keeps its color across measures and ranges, in
  the order of their names, checked for color blindness and contrast in
  light and dark mode.
- **Tooltips.** Hovering over a chart, or focusing it with Tab and using the
  arrow keys, Home and End, shows the time of the nearest point and every
  sensor's mean, lowest and highest there. Without the script, the table
  under each chart and the downloads give the same values.

### Downloads

CSV files with a header row, oldest first, every nick together:

- **The points shown** and **hourly points:** `time` (the point's start,
  as the logs show it), `sensor`, `measure`, `readings`, `mean`, `low`,
  `high`.
- **Every reading**, for a day, a week or a month: `time` (as the logs show
  it), `utc`, `sensor`, `measure`, `value`. Longer ranges would be hundreds
  of megabytes, so they offer the hourly points instead.

## Code

The pages live in `src/review/`, built on the server with
[maud](https://maud.lambda.xyz/) and served with
[axum](https://github.com/tokio-rs/axum). Charts are drawn in
`src/review/chart.rs` from the points `src/graphs.rs` reads, and
`src/review/chart.js` adds their tooltips. Each request reads the database on
its own connection; the write-ahead log lets pages read while an analysis
writes. The server and the Analyze page come from wot's `wot review`.
