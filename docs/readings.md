# Readings

## What counts as a reading

After importing, `sensors analyze` reads every channel message that holds a
JSON object, such as:

```text
13:52 < greenhouse> {"sensor":"inside","temp":"67.46","humid":"38.80","timestamp":"2026-11-03T20:52:43.986Z"}
```

- **`sensor`** names the sensor, here `inside`. Lines without it are
  readings from the nick itself, and show `-` for the sensor.
- **`timestamp`** says when, as an RFC 3339 time such as
  `2026-11-03T20:52:43.986Z`.
- **Every other field** holding a number, or a string that is one such as
  `"67.46"`, is a reading of the measure it is named after: here `temp` and
  `humid`. Fields holding anything else, such as `true` or `"ok"`, are left
  out.

A new sensor or measure needs no setup: a soil moisture sensor posting
`{"sensor":"bed1","moisture":"41.2","timestamp":...}` shows up as sensor
`bed1` with a `moisture` measure.

Each measure from each sensor is a **series**, named by its channel, nick,
sensor and measure.

## Markers

A JSON object with a timestamp and no readings, such as
`{"testBegin":true,"timestamp":"2026-11-03T20:45:23.061Z"}`, is kept as a
marker, to show on graphs as the moment something happened. So is a line
whose values are all blank or not numbers, such as a sensor reporting an
error.

## What is left out

- **Messages that are not JSON objects**, such as people talking in the
  channel.
- **Lines without a usable timestamp.** They are counted after each run as
  `Left out N JSON lines without a timestamp`.
- **A second reading of the same measure at the same moment**, such as a
  line posted twice. They are counted as `Left out N readings at the same time
  as another of the same measure`.

## Faulty readings

Sensors sometimes report a value their hardware made up, from RF
interference, moisture or condensation on the circuit, rather than one they
measured. These faulty readings are found as the readings are built, kept
apart, and left out of every graph line, hourly total, mean, lowest, highest
and latest reading. A reading is faulty when it is:

- **Out of range:** outside what the measure can plausibly be in the field.
  A temperature sensor that fails often reports the end of its scale, such
  as -50 °C or 150 °C, which is -58 °F or 302 °F.
- **A spike:** further than the measure's spike size from the median of
  itself and its neighbors, up to two readings each side within three
  minutes. A reading that jumps away and comes straight back is a spike; a
  change that lasts, such as a heater turning on, is not, since the readings
  after it agree with it.

| Measure | Plausible range | Spike size |
| --- | --- | --- |
| `temp` | -40 to 140 °F | 10 °F |
| `humid` | 0 to 100 % RH | 15 % RH |

Other measures are not checked. The rules are in `src/faults.rs`; after
changing them, run `sensors analyze --rebuild` to check every reading again.

A reading needs a neighbor after it to be a spike, so the newest reading is
checked again when the next one arrives. Each run says how many faulty
readings there are in all: `Flagged N faulty readings in all, left out of
graphs`.

## Time zones

Timestamps are UTC, but irssi logs in the logging computer's time zone. Each
line's timestamp, compared with the minute irssi logged it, gives the logs'
offset from UTC, rounded to a quarter hour: a line logged at 13:52 with the
timestamp 20:52:43 UTC is from UTC-7. The offset most lines had in each hour
is the one used, so times are shown as the logs show them, across daylight
saving changes, with no time zone to set.

## Building

Readings are read from the lines imported since the last run, so a run that
imported nothing new reads nothing. Everything is read again when a log file
was replaced, or with `--rebuild`:

```bash
sensors analyze --rebuild ~/irclogs
```

Each hour's count, mean, lowest and highest value are kept too, so graphs of
months or years read a row an hour instead of every reading. The same pass
counts the messages and actions said in each channel each day, for the
[History](review.md#pages) page.

## The sensor list

`sensors analyze` ends by listing every series, with its readings and when
they start and end, in the logs' time zone:

```text
Channel      Nick        Sensor   Measure  Readings  From and to
#greenhouse  greenhouse  inside   humid       2,864  2026-11-03 13:46 to 2026-11-04 18:06
#greenhouse  greenhouse  inside   temp        2,864  2026-11-03 13:46 to 2026-11-04 18:06
#greenhouse  greenhouse  outside  humid       2,864  2026-11-03 13:46 to 2026-11-04 18:06
#greenhouse  greenhouse  outside  temp        2,864  2026-11-03 13:46 to 2026-11-04 18:06
```

Progress and summaries go to standard error, so the list can be saved on its
own with `sensors analyze ~/irclogs > sensors.txt`.
