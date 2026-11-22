# NOTE: This Dockerfile depends on you building the rustak-plugin-remoteid
# binary first. It will then package that binary into the image, and use that
# as the entrypoint. This means that running `docker build` is not a
# repeatable way to build the same image, but the benefit is much faster
# cross-platform builds; a net win.
FROM ubuntu:24.04

LABEL org.opencontainers.image.source=https://github.com/SierraSoftworks/rustak
LABEL org.opencontainers.image.description="Remote ID drones (with a spoof verdict) from a pcap or RIDF capture, as CoT, for rustak."
LABEL org.opencontainers.image.licenses=MIT

RUN apt-get update && apt-get install -y --no-install-recommends \
  ca-certificates \
  && rm -rf /var/lib/apt/lists/*

ADD ./dist/rustak-plugin-remoteid /usr/local/bin/rustak-plugin-remoteid

# A sidecar dials out and listens on nothing, so there is no port to expose.
# /data holds the configuration file, the certificate and key, and the capture
# the receiver writes.
VOLUME /data

ENTRYPOINT ["/usr/local/bin/rustak-plugin-remoteid", "--config", "/data/plugin.toml"]
