# Live graphs

```bash
sensors live --time-zone America/Denver
sensors live --time-zone America/Denver --nick greenhouse-graphs --listen 0.0.0.0:8080
```

`sensors live` is an IRC bot. It joins `#greenhouse`, logs the channel the
way irssi would, reads each reading into the database within a few seconds
of a sensor posting it, and serves the Sensors pages of
[`sensors review`](review.md) to anyone who can reach it, at
<http://127.0.0.1:7173/> by default. It runs until you press Ctrl+C, or
Docker stops it, then quits IRC with "brb, getting a software update".

It never says anything on IRC. It answers CTCP requests such as VERSION,
and takes its nick back when whoever had it leaves.

## Options

Every option can also be set in an environment variable, which is how a
container is given them.

| Option | Variable | Default | |
| --- | --- | --- | --- |
| `--time-zone` | `SENSORS_TIME_ZONE` | none | The time zone to write the logs in, such as `America/Denver`. Required. |
| `--server` | `SENSORS_SERVER` | `irc.wetfish.net` | The IRC server, always reached over TLS |
| `--port` | `SENSORS_PORT` | `6697` | The server's TLS port |
| `--nick` | `SENSORS_NICK` | `sensors` | Its nick. When it is taken, it uses `sensors_` or `sensors__` until it is free. |
| `--channel` | `SENSORS_CHANNELS` | `#greenhouse` | A channel to join, log and process. Repeat it for more, or separate them with commas in the variable. |
| `--network` | `SENSORS_NETWORK` | `wetfish` | The network's folder in the logs, as irssi names it after the network's tag |
| `--logs` | `SENSORS_LOGS` | `live-logs` in the repository folder | The log folder it writes and imports |
| `--db` | `SENSORS_DB` | `sensors.sqlite` in the repository folder | The database |
| `--listen` | `SENSORS_LISTEN` | `127.0.0.1:7173` | Where the live site listens. `0.0.0.0:8080` listens on every network, as in a container. |

The channels replace the saved ones, as `sensors analyze --channel` does,
so the lines of other channels are removed from the database. See
[Channels to process](logs.md).

## Logs

The bot writes `<logs>/<network>/<channel>.log`, such as
`live-logs/wetfish/#greenhouse.log`, in irssi's default format: what is
said, including with `/me`, who joins, leaves or is kicked, and the lines
irssi writes when a log is opened or closed and when the day changes. It
does not log quits or nick changes, which irssi writes to every channel the
person was in, since the bot doesn't keep track of who is in its channels.

Times are written in `--time-zone`, as irssi writes them in its computer's.
The graphs then work out the logs' UTC offset from them, as they do for
irssi's logs (see [Time zones](readings.md#time-zones)), so the live graphs
show times as clocks there show them, changing with daylight saving time.
Use the time zone of the irssi whose logs you also import, so the two agree.

Each time lines are logged, the analysis reads them, two seconds later, so
both sensors' readings are read together. It does what `sensors analyze`
does with the log folder: imports the new lines, then reads the readings in
them, checking them for faults. The first analysis after starting shows its
progress and a summary; later ones are quiet unless something goes wrong.

Keeping logs, rather than writing readings straight to the database, means
the database can be built again from them at any time, as from irssi's, and
`sensors review` on the same database shows the bot's lines in the History.

## Bringing in older readings

The readings from before the bot joined come from the logs of an irssi
that was in the channel. Copy its log files into the bot's log folder,
under a network folder of their own, so they never mix with the bot's:

```text
live-logs/
  wetfish/#greenhouse.log      written by the bot
  irssi/#greenhouse.log        copied from irssi's wetfish folder
```

The next analysis imports them, which may take a while for years of logs,
and graphs both together: a reading in both logs counts once (see
[One logger per channel](logs.md#one-logger-per-channel)). Copy a log
again later, once it has grown, and only the new lines are read.

## The live site

The live site shows the Sensors pages, with the newest week at `/`, the
close-ups, and the CSV downloads, and nothing else: no History, which would
show everything said in the channels, and no Analyze, which the bot does
by itself. Fault marks open close-ups as in the review, without links to
the History. Its footer links to the source code, which the AGPL asks a
site running it to offer.

Each sensor's newest readings are at the top, with how long ago they were
taken. A page showing the newest period reloads itself every 30 seconds to
show new readings, keeping its place on the page. Its script waits while a
chart is pointed at or has focus, so a tooltip or fault preview is never
cut short, and while the page is in a hidden tab. Without the script, it
reloads every 30 seconds regardless. An earlier period never reloads.

`/up` answers `200 OK` while the bot is on IRC and in every one of its
channels, and `503 Service Unavailable` while it isn't, never cached.
`sensors health ADDRESS` asks it, for a container's health check, since a
small container has no curl. [Deploying](deploying.md) uses it for the
container's health check:

```bash
sensors health 127.0.0.1:8080 && echo up
```
