# Database

`sensors` keeps everything in one SQLite file, `sensors.sqlite` in the
repository folder by default, which `.gitignore` leaves out. `--db` uses
another file. Open it with any SQLite client, such as
`sqlite3 sensors.sqlite`.

Everything in it is read from the logs, so deleting the file and running
`sensors analyze` again rebuilds it.

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

Dates are `YYYY-MM-DD` and times `HH:MM`, in the logs' own time zone. A
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
