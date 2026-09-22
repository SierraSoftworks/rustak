# NOTE: This Dockerfile depends on you building the rustak-plugin-esb binary
# first. It will then package that binary into the image, and use that as the
# entrypoint. This means that running `docker build` is not a repeatable way to
# build the same image, but the benefit is much faster cross-platform builds; a
# net win.
FROM ubuntu:24.04

LABEL org.opencontainers.image.source=https://github.com/SierraSoftworks/rustak
LABEL org.opencontainers.image.description="ESB power-outage sidecar for rustak: PowerCheck outages as CoT markers."
LABEL org.opencontainers.image.licenses=MIT

RUN apt-get update && apt-get install -y --no-install-recommends \
  ca-certificates \
  && rm -rf /var/lib/apt/lists/*

ADD ./dist/rustak-plugin-esb /usr/local/bin/rustak-plugin-esb

# A sidecar dials out and listens on nothing, so there is no port to expose.
# /data holds the configuration file, the certificate and key, and a replay
# fixture if one is used.
VOLUME /data

ENTRYPOINT ["/usr/local/bin/rustak-plugin-esb", "--config", "/data/plugin.toml"]
