# Reviewing in a browser

```bash
sensors review
sensors review --port 8080 --db /path/to/other.sqlite
```

`sensors review` serves the review pages at <http://127.0.0.1:7172/> until
you press Ctrl+C. The port differs from `wot review`'s 7171, so both can run
at once. It only listens on 127.0.0.1, so only this computer can open it,
and it has no login. Pages are plain HTML, in your system's light or dark
mode, and work without JavaScript.

## Pages

- **Overview:** how many log files and lines were imported and when, and
  each sensor's measures with their latest reading, when it was taken, how
  many readings there are, and since when. Times are as the logs show them,
  see [Time zones](readings.md#time-zones). **Analyze now** reads new lines.
- **History:** the lines said in channels one day at a time, the way irssi
  shows them, newest first, 200 to a page. It opens on the most recent day,
  with filters for a channel, a day and text the lines contain. Below them,
  every day with lines, by month, links to that day. Private messages are
  left out.
- **Analyze:** the log folders the imported files came from, and
  **Analyze now**, which does what `sensors analyze` does with them. Tick
  **Read the readings from every line again** to do what `--rebuild` does.
  It runs in the background, and the page reloads itself with a progress bar
  for each step until it is done. Other pages keep working meanwhile,
  showing the data from before it, with a banner. New network folders inside
  the log folders are found too, but a log folder never imported needs
  `sensors analyze FOLDER` once.

## Code

The pages live in `src/review/`, built on the server with
[maud](https://maud.lambda.xyz/) and served with
[axum](https://github.com/tokio-rs/axum). Each request reads the database on
its own connection; the write-ahead log lets pages read while an analysis
writes. The server and the Analyze page come from wot's `wot review`.
