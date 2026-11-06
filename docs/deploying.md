# Deploying

`sensors live` runs on a [Wetfish server](https://github.com/wetfish/server)
as a project named `sensors`, on `sensors.wildland.eco`, deploying the
`main` branch whenever it is pushed. There is no staging copy.

## What runs

- **One container**, `sensors-sensors-1`, built from the `Dockerfile`. It
  runs [`sensors live`](live.md) as an unprivileged user, `sensors` (uid
  10001), and can reach nothing on the server but its own volume.
- **The live site** on port 8080, which the server's Caddy serves on
  `https://sensors.wildland.eco/`. The container's health check asks its
  `/up` with `sensors health`, so a deploy only succeeds once the bot is in
  `#greenhouse`.
- **The deploy webhook**, at `https://<server hostname>/hooks/sensors` on
  the server's own hostname (`SERVER_HOSTNAME` in the server's `.env`), not
  on `sensors.wildland.eco`. The server only answers `/hooks/` on its own
  hostname; every other domain goes straight to its project.
- **The `data` volume**, Docker's `sensors_data`, at `/data` in the
  container, with the database, `sensors.sqlite`, and the logs, in `logs/`.
- **The settings**, `config/sensors/sensors.env` on the server, which every
  deploy copies into the project's clone as `sensors.env`, for Compose to
  pass to the container. `setup.conf` asks for the time zone and the nick
  when the project is added, offering the values in `sensors.env.example`.
  The container's own paths and port are set in `docker-compose.yml`.

When a deploy replaces the container, the bot quits IRC with "brb, getting
a software update" and the new container joins again, which takes a few
seconds. Readings posted meanwhile are missing from its logs; see
[Bringing in older readings](#bringing-in-older-readings) to fill gaps
from an irssi that stayed in the channel.

## Adding sensors to a server

The commands run as root on the server, in `/srv/wetfish-server`.

1. Point `sensors.wildland.eco` at the server in DNS, so Caddy can get its
   certificate.

2. Add the project, deploying `main` rather than a production server's
   usual `release`:

   ```bash
   ./add-project.sh --name sensors --repo https://github.com/Wildland-Ecotech/sensors.git \
       --domain sensors.wildland.eco --branch main --upstream sensors:8080
   ```

   It asks for the time zone to show times in, such as `America/Denver`,
   and the bot's nick, offering `sensors`. Use a nick no other copy of the
   bot uses, such as one running on your computer, so the two don't take it
   from each other. On a server with [storage](#storage), it also asks which
   volumes to keep there; none is needed.

   It then builds sensors, starts it, and waits for it to join
   `#greenhouse`. The first build compiles every dependency, which can take
   several minutes on a small server. At the end it prints the webhook to
   add in GitHub.

3. Check that `https://sensors.wildland.eco/` answers, and follow what the
   bot does:

   ```bash
   docker logs -f sensors-sensors-1
   ```

   The page says there are no readings yet until the greenhouse posts one,
   within thirty seconds, unless you bring in older readings.

4. Add the webhook in GitHub, under the repository's Settings → Webhooks →
   Add webhook, with what `add-project.sh` printed:

   | Field | Value |
   | --- | --- |
   | Payload URL | `https://<server hostname>/hooks/sensors` |
   | Content type | `application/json` |
   | Secret | The webhook secret it printed, also in `config/sensors.conf` as `WEBHOOK_SECRET` |
   | Events | Just the push event |

   GitHub sends a ping when the webhook is added. Under the webhook's Recent
   Deliveries, it answers 200 with `Ignored a request without a ref: this
   server deploys main of sensors`, which shows the secret matches. A push
   to `main` answers `Deploy of sensors main requested` and deploys; pushes
   to other branches are ignored.

If the first deploy fails, for example because the time zone isn't one,
`docker logs sensors-sensors-1` says why. The project stays added; fix the
problem and deploy again with `./deploy.sh sensors`.

## Bringing in older readings

The readings from before the bot joined are in the logs of the irssi on
your computer. Copy its `#greenhouse` log into the volume, under a network
folder of its own (see [Live graphs](live.md#bringing-in-older-readings)):

```bash
scp ~/irclogs/wetfish/'#greenhouse.log' root@<server>:/root/greenhouse.log
```

Then on the server:

```bash
logs="$(docker volume inspect --format '{{.Mountpoint}}' sensors_data)/logs"
install -d -o 10001 -g 10001 "$logs/irssi"
install -o 10001 -g 10001 -m 644 /root/greenhouse.log "$logs/irssi/#greenhouse.log"
```

The bot imports it the next time it reads its own lines, within thirty
seconds, which can take a while for years of logs; `docker logs` shows the
progress. Readings both logs have count once. Copying the log again later
fills any gaps, such as from deploys, and only its new lines are read.

## Changing the settings

Edit `config/sensors/sensors.env` and deploy:

```bash
./deploy.sh sensors
```

When nothing else changed, the deploy leaves the container as it is, so
restart it to use the new settings, which rejoins IRC:

```bash
docker restart sensors-sensors-1
```

The settings it can take are in [Live graphs](live.md#options). Changing
`SENSORS_CHANNELS` removes the other channels' lines from the database, as
`sensors analyze --channel` does.

## Updates

Pushing to `main` deploys, through the webhook. To deploy by hand, or
follow a deploy:

```bash
./deploy.sh sensors
journalctl -u project-deploy@sensors -f
```

To go back to an earlier commit, deploy it, then push a fix or a revert so
the next push doesn't bring the change back:

```bash
./deploy.sh sensors 3f9c2a1b7d4e
```

A newer sensors may have upgraded the database, which an older one refuses.
Since the database can be built again from the logs, the simplest way back
is to delete it while the container is stopped; the older sensors builds a
new one from the logs when it starts:

```bash
docker stop sensors-sensors-1
rm "$(docker volume inspect --format '{{.Mountpoint}}' sensors_data)"/sensors.sqlite*
./deploy.sh sensors 3f9c2a1b7d4e
```

## Backups

The logs are what matter: the database can always be built again from
them. Copy the volume's `logs` folder, which is safe while the bot runs:

```bash
cp -a "$(docker volume inspect --format '{{.Mountpoint}}' sensors_data)/logs" /root/sensors-logs-$(date +%F)
```

## Storage

The volume grows slowly, by a megabyte or two a day of logs and readings,
well under a gigabyte a year, so it can stay on the root disk. To keep it on the server's
[storage](https://github.com/wetfish/server/blob/main/docs/how-it-works.md#storage)
instead, add it with `./storage.sh add sensors data`, then make its folder
writable by the container's user, as the server's docs describe:

```bash
chown -R 10001:10001 /mnt/virtual/<server hostname>/sensors/data
```

## Trying it in Docker on your computer

`docker-compose.override.yml`, which deploys leave out, publishes the live
site on `http://localhost:8080/`. Put your settings in `sensors.env`, which
`.gitignore` excludes, using a nick of its own, then:

```bash
cp sensors.env.example sensors.env
docker compose up --build
```

The database and logs are then in the `data` volume of the folder's Compose
project, not in your `sensors.sqlite` or `live-logs`.
