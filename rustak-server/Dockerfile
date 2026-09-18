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

ENTRYPOINT ["/usr/local/bin/rustak", "--config", "/data/config.toml"]
