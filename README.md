# sensors

An open source nature sensor network.

`sensors` reads the readings that sensors post to IRC, as JSON lines in
irssi chat logs, so they can be graphed. Wildland Ecotech's greenhouse posts a
reading from each of its sensors to `#greenhouse` every thirty seconds:

```text
18:03 < greenhouse> {"sensor":"outside","temp":"49.46","humid":"58.80","timestamp":"2026-11-05T01:03:07.195Z"}
18:03 < greenhouse> {"sensor":"inside","temp":"51.44","humid":"49.60","timestamp":"2026-11-05T01:03:07.248Z"}
```

The logs go into a local SQLite database, and `sensors review` serves a web
interface for them that only your computer can open. `sensors live` is an
IRC bot that logs the channel itself and serves live graphs to anyone, for
running on a server.

## Status

Early development. It imports irssi logs into the database, reads the
sensor readings in them, flags faulty readings from hardware glitches, and
shows them in a web interface: charts of each measure over a day, week,
month, year or everything, with CSV downloads, and the raw log lines by day.
`sensors live` follows the channel on IRC and graphs readings within
seconds.

## Install

You need Rust 1.89 or newer and a C compiler. SQLite is built in.

```bash
cargo install --path .
```

This puts `sensors` on your `PATH`. `cargo build --release` builds it to
`target/release/sensors` instead.

## Quick start

Import your irssi log folders, one per irssi instance:

```bash
sensors analyze ~/irclogs
```

Only `#greenhouse` is imported, unless you name other channels with
`--channel '#name'`, which is saved for later runs. The first run imports
the whole channel and can take a while for years of logs; it shows each step
as it goes. Later runs only read the lines added since. Each
run reads the readings in the new lines and ends with a list of every
sensor's measures:

```text
Channel      Nick        Sensor   Measure  Readings  From and to
#greenhouse  greenhouse  inside   humid       2,864  2026-11-03 13:46 to 2026-11-04 18:06
#greenhouse  greenhouse  inside   temp        2,864  2026-11-03 13:46 to 2026-11-04 18:06
```

Then review them in your browser:

```bash
sensors review
```

Open <http://127.0.0.1:7172/>. **Analyze now** on the Overview reads new
lines later, so the command line is only needed for the first import.

## Live graphs

To graph readings as they are posted, run the bot, with the time zone your
logs should be in:

```bash
sensors live --time-zone America/Denver
```

It joins `#greenhouse` on `irc.wetfish.net` as `sensors`, logs it into
`live-logs/` as irssi would, and serves the graphs at
<http://127.0.0.1:7173/>, without the History or Analyze pages. See
[Live graphs](docs/live.md) for its options and bringing in older logs. It
runs at <https://sensors.wildland.eco/>, deployed with Docker on a Wetfish
server, see [Deploying](docs/deploying.md).

## Documentation

- [Logs and importing](docs/logs.md): the log layout `sensors` expects, the
  channels to process, and how importing works.
- [Readings](docs/readings.md): which JSON lines are readings, markers,
  faulty readings, time zones and rebuilding.
- [Reviewing in a browser](docs/review.md): the pages of `sensors review`,
  and how the graphs and downloads work.
- [Live graphs](docs/live.md): the `sensors live` bot, its logs, and its
  site.
- [Deploying](docs/deploying.md): running `sensors live` on a Wetfish
  server with Docker, at `sensors.wildland.eco`.
- [Database](docs/database.md): the tables, for querying them or working on
  `sensors`.

## Credits

The log importer and the review server come from
[wot](https://github.com/wetfish/web-of-trust), the web of trust tools for
IRC communities by Wetfish, and the IRC connection of `sensors live` from
[lurker](https://github.com/wetfish/lurkers), Wetfish's bot for idle users.

## License

AGPL-3.0, see [LICENSE](LICENSE).
