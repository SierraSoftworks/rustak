# NOTE: This Dockerfile depends on you building the rustak-plugin-example
# binary first. It will then package that binary into the image, and use that
# as the entrypoint. This means that running `docker build` is not a
# repeatable way to build the same image, but the benefit is much faster
# cross-platform builds; a net win.
FROM ubuntu:24.04

LABEL org.opencontainers.image.source=https://github.com/SierraSoftworks/rustak
LABEL org.opencontainers.image.description="Example TAK-native sidecar plugin for rustak."
LABEL org.opencontainers.image.licenses=MIT

RUN apt-get update && apt-get install -y --no-install-recommends \
  ca-certificates \
  && rm -rf /var/lib/apt/lists/*

ADD ./rustak-plugin-example /usr/local/bin/rustak-plugin-example

VOLUME /data
EXPOSE 443 8446 8443 8089 8087

ENTRYPOINT ["/usr/local/bin/rustak-plugin-example", "--config", "/data/plugin.toml"]
