# NOTE: This Dockerfile depends on you building the rustak binary first.
# It will then package that binary into the image, and use that as the entrypoint.
# This means that running `docker build` is not a repeatable way to build the same
# image, but the benefit is much faster cross-platform builds; a net win.
FROM ubuntu:24.04

LABEL org.opencontainers.image.source=https://github.com/SierraSoftworks/rustak
LABEL org.opencontainers.image.description="A lightweight, Rust-native TAK server."
LABEL org.opencontainers.image.licenses=MIT

RUN apt-get update && apt-get install -y --no-install-recommends \
  ca-certificates \
  && rm -rf /var/lib/apt/lists/*

ADD ./dist/rustak /usr/local/bin/rustak

VOLUME /data
EXPOSE 443 8446 8443 8089 8087

# Explicit, although SIGTERM is also the default: rustak's shutdown is two
# bounded waits, and `docker stop` has to allow for both of them.
#
#   1. the drain — `[server] shutdown_timeout`, 8s by default — during which the
#      HTTP, Marti and CoT stream listeners let open connections close; then
#   2. two seconds, fixed, for the WAL `TRUNCATE` checkpoint, which is what
#      leaves the volume with a database rather than a database plus a log.
#
# `docker stop` waits ten seconds and then SIGKILLs, so the defaults fit with
# nothing to spare. RAISING `[server] shutdown_timeout` MEANS RAISING THE STOP
# TIMEOUT WITH IT — `docker stop -t <shutdown_timeout + 2>`, or `stop_grace_period`
# in compose — or the checkpoint is the part that gets killed. A second SIGTERM
# ends the drain early and still runs the checkpoint.
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/local/bin/rustak", "--config", "/data/config.toml"]
