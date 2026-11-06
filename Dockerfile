# Builds sensors, then copies it into a small image of its own that runs
# sensors live. See docs/deploying.md.

FROM rust:1.99.0-slim-trixie AS build
WORKDIR /src

# Build the dependencies on their own first, so a change to sensors' code
# reuses them from Docker's cache
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src src
# Newer than the placeholder built above, so cargo rebuilds sensors itself
RUN touch src/main.rs && cargo build --release --locked

FROM debian:trixie-20261005-slim
COPY --from=build /src/target/release/sensors /usr/local/bin/sensors

# Runs as a user of its own, which owns /data, so a new data volume, which
# starts as a copy of it, is writable
RUN useradd --system --uid 10001 --user-group --no-create-home --home-dir /data sensors \
    && mkdir /data \
    && chown sensors:sensors /data
USER sensors
WORKDIR /data

# The settings come from the environment; see docker-compose.yml
ENTRYPOINT ["sensors"]
CMD ["live"]
