# NOTE: This Dockerfile depends on you building the rustak-plugin-adsb binary
# first. It will then package that binary into the image, and use that as the
# entrypoint. This means that running `docker build` is not a repeatable way to
# build the same image, but the benefit is much faster cross-platform builds; a
# net win.
FROM ubuntu:24.04

LABEL org.opencontainers.image.source=https://github.com/SierraSoftworks/rustak
LABEL org.opencontainers.image.description="ADS-B feed sidecar for rustak: open aircraft tracking as CoT."
LABEL org.opencontainers.image.licenses=MIT

RUN apt-get update && apt-get install -y --no-install-recommends \
  ca-certificates \
  && rm -rf /var/lib/apt/lists/*

ADD ./dist/rustak-plugin-adsb /usr/local/bin/rustak-plugin-adsb

# A sidecar dials out and listens on nothing, so there is no port to expose.
# /data holds the configuration file, the certificate and key, and a replay
# fixture if one is used.
VOLUME /data

ENTRYPOINT ["/usr/local/bin/rustak-plugin-adsb", "--config", "/data/plugin.toml"]
