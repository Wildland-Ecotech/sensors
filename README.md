# sensors

An open source nature sensor network.

`sensors` reads the readings that sensors post to IRC, as JSON lines in
irssi chat logs, so they can be graphed. Wildland Ecotech's greenhouse posts a
reading from each of its sensors to `#greenhouse` every thirty seconds:

```text
18:03 < greenhouse> {"sensor":"outside","temp":"49.46","humid":"58.80","timestamp":"2026-11-05T01:03:07.195Z"}
18:03 < greenhouse> {"sensor":"inside","temp":"51.44","humid":"49.60","timestamp":"2026-11-05T01:03:07.248Z"}
```

Everything runs on your own computer. The logs go into a local SQLite
database.

## Status

Early development. It imports irssi logs into the database. Next are the
readings in those lines, then a web interface with graphs of them.

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

The first run imports everything and can take a while for years of logs; it
shows each step as it goes. Later runs only read the lines added since.

## Documentation

- [Logs and importing](docs/logs.md): the log layout `sensors` expects and
  how importing works.
- [Database](docs/database.md): the tables, for querying them or working on
  `sensors`.

## Credits

The log importer comes from [wot](https://github.com/wetfish/web-of-trust),
the web of trust tools for IRC communities by Wetfish.

## License

AGPL-3.0, see [LICENSE](LICENSE).
