# Database

`sensors` keeps everything in one SQLite file, `sensors.sqlite` in the
repository folder by default, which `.gitignore` leaves out. `--db` uses
another file. Open it with any SQLite client, such as
`sqlite3 sensors.sqlite`.

Everything in it is read from the logs, apart from the channels to process,
so deleting the file and running `sensors analyze` again rebuilds it; name
the channels again with `--channel` if they were not only `#greenhouse`.

## Upgrades

The file records its schema version. A newer `sensors` upgrades an older file
the next time it opens it, which can take a few minutes for a large one, and
refuses a file from a newer `sensors` than itself.

## Write-ahead log

The file uses SQLite's write-ahead log, so readers keep seeing the data from
before an import while it writes. While `sensors` runs,
`sensors.sqlite-wal` and `sensors.sqlite-shm` sit next to it: back up all
three, or only `sensors.sqlite` once `sensors` has stopped. A write waits up
to 5 seconds for another one to finish.

## Tables

| Table | One row per | Main columns |
| --- | --- | --- |
| `files` | log file | `path`, `tag` (its network tag folder), `target`, `kind` (`channel` or `query`), how far it was read |
| `events` | log line | `file_id`, `line`, `date`, `time`, `kind`, `nick`, `ident`, `host`, `other_nick`, `text`, `detail` |
| `sensor_series` | measure of a sensor | `id`, `tag` (the folder it was first found in; one series holds every tag's readings), `channel`, `nick`, `sensor` (`''` when the lines have none), `measure` |
| `readings` | reading | `series_id`, `at`, `value` |
| `reading_hours` | series and hour | `series_id`, `hour`, `readings`, `total` (their sum), `low`, `high` |
| `reading_markers` | JSON line without readings | `tag`, `channel`, `nick`, `at`, `text` |
| `reading_faults` | faulty reading | `series_id`, `at`, `value`, `reason` (`range` or `spike`) |
| `log_offsets` | hour and UTC offset | `hour`, `minutes` (ahead of UTC, such as `-420`), `lines` seen with it |
| `reading_state` | (one row) | `last_event_id`, the last event read for readings |
| `channel_days` | channel and day | `tag`, `channel`, `date`, `lines`: messages and actions said |
| `channels` | channel to process | `name`, compared ignoring case; `#greenhouse` in a new database |

The reading tables and `channel_days` are built from the events after each
import, as [Readings](readings.md) describes. `channels` is set by
`sensors analyze --channel`, see [Channels](logs.md#channels). `at` is
milliseconds since 1970 UTC, and `hour` is hours since 1970 UTC,
`at / 3600000`. A mean over any stretch of hours is
`sum(total) / sum(readings)`.

`reading_hours` leaves out the readings in `reading_faults`, while
`readings` keeps them. A query of `readings` that should leave them out
needs `NOT EXISTS (SELECT 1 FROM reading_faults f WHERE f.series_id =
r.series_id AND f.at = r.at)`.

## Events

In `events`, dates are `YYYY-MM-DD` and times `HH:MM`, in the logs' own time zone. A
sensor's JSON is the `text` of a `message` event. Which event columns are
filled depends on its `kind`:

| Kind | `nick` | `ident`, `host` | `other_nick` | `text` | `detail` |
| --- | --- | --- | --- | --- | --- |
| `message` | speaker | | | message | channel status, such as `@` |
| `action` | speaker | | | action | |
| `notice` | sender | | | notice | |
| `server_notice` | | | | notice | server |
| `join` | nick | yes | | | channel |
| `part` | nick | yes | | reason | channel |
| `quit` | nick | yes | | reason | |
| `kick` | kicked nick | | kicked by | reason | channel |
| `nick_change` | old nick | | new nick | | |
| `own_nick_change` | | | new nick | | |
| `topic` | nick | | | topic | channel |
| `topic_unset` | nick | | | | channel |
| `mode`, `server_mode` | set by | | | modes | channel |
| `netsplit` | | | | nicks | servers |
| `netsplit_over` | | | | nicks | |
| `client` | | | | irssi's message | |
| `log_opened`, `log_closed` | | | | | |
| `unparsed` | | | | the whole line | |

`log_opened` and `log_closed` have their time as `HH:MM:SS`, and `unparsed`
lines have none.
