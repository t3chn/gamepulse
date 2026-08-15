FROM rust:1.97.1-bookworm AS build

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p gamepulse

FROM debian:bookworm-slim

RUN groupadd --gid 10001 gamepulse \
    && useradd --uid 10001 --gid gamepulse --create-home --home-dir /app gamepulse \
    && install --directory --owner gamepulse --group gamepulse /var/lib/gamepulse

COPY --from=build --chown=gamepulse:gamepulse /workspace/target/release/gamepulse /usr/local/bin/gamepulse

USER gamepulse
ENV GAMEPULSE_SOURCE_WORK_ENABLED=true
VOLUME ["/var/lib/gamepulse"]
EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/gamepulse"]
