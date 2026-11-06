# Logs and importing

## Log layout

`sensors analyze` takes one log folder per irssi instance, laid out the way
irssi's default `autolog_path` writes logs:

```ini
autolog_path = ~/irclogs/$tag/$0.log
```

That gives a folder per network tag, with a `.log` file per channel or
private message:

```text
irclogs/
  wetfish/
    #greenhouse.log
```

Files starting with `#`, `&`, `!` or `+` are channels; other `.log` files are
private messages. Anything else is skipped, and each run lists what it
skipped and why.

## Channels

Only the log files of the channels to process are imported: `#greenhouse`
unless you name others. Other channels and private messages are left out,
and each run says how many files it left out. Name the channels with
`--channel`, once for each, quoted, since the shell takes `#` for the start
of a comment:

```bash
sensors analyze --channel '#greenhouse' --channel '#garden' ~/irclogs
```

The channels are saved in the database, so later runs, and **Analyze now**
in `sensors review`, process the same ones without naming them again.
Channel names match ignoring case.

When the channels change, the next run removes the files and lines of
channels no longer named, and reads the readings again from the lines that
remain. The database file keeps its size until `sensors compact` rewrites
it, which needs free disk space about its size:

```bash
sensors compact
```

## Log format

`sensors` expects irssi's default theme and log settings:

```ini
log_timestamp = %H:%M
log_open_string = --- Log opened %a %b %d %H:%M:%S %Y
log_close_string = --- Log closed %a %b %d %H:%M:%S %Y
log_day_changed = --- Day changed %a %b %d %Y
```

Lines only carry the time; the date comes from the last `Log opened`,
`Log closed` or `Day changed` line above them. Every line is imported, and
lines `sensors` does not recognize are kept as unparsed, so new formats show
up instead of being ignored.

## Importing

The first `sensors analyze` imports every line into the database. Later runs
only read what changed:

- A file that grew is read from where the last import stopped.
- A file that shrank, or whose start changed, was replaced, and is imported
  again from the start.
- A last line without a newline, which irssi may still be writing, waits for
  the next run.

Each file is imported in its own transaction, so an interrupted run carries on
where it stopped next time. A file that cannot be read is listed after the
import, and the other files are still imported. Progress shows as it goes:

```text
Importing log files: checked 3 files (1m 12s)
Database: /home/rachel/sensors/sensors.sqlite
Imported 5,729 new lines: 1 new file, 2 with new lines, 0 replaced, 0 unchanged
```

## One logger per channel

`sensors` expects each channel to be logged once. Unlike wot, it does not
link network tags into networks, read a bouncer's playback, or leave out
lines logged twice by two clients, so the History shows such a line twice.
Readings are the exception: a channel's readings from every network tag
folder make one series, so a reading two loggers logged, such as an irssi
and [`sensors live`](live.md), counts once.
