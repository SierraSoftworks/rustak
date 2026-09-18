# syntax=docker/dockerfile:1

# ATAK's own networking core (commoncommo) and its stock `commotest` console
# client, built from upstream source at image-build time.
#
# Nothing from atak-civ is vendored into this repository: this image is the
# only place GPL code exists in the rustak interop story, and even here it is
# fetched, never committed. See README.md for the licence posture and for what
# the resulting harness can and cannot cover.
#
# Build context is this directory:
#     docker build --progress=plain -t rustak-interop-commoncommo interop/eud
# CI builds it weekly and on demand — see .github/workflows/nightly.yml, job
# `interop-eud-image`, which derives the published image tag from the pin below.

# Upstream pin: TAK-Product-Center/atak-civ, tag 5.5.1.10.
# Pin the commit, not the tag: 5.5.1.10 is not a direct commit tag, so
# `git clone --branch` warns while still landing on this revision.
# Bump deliberately — re-read `commotest -h` when you do, since the CLI is an
# internal test tool with no stability promise.
ARG ATAK_CIV_SHA=9f6893dd657feacc35ec5de03dad721c2e44170e


FROM debian:bookworm-slim AS builder
SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

# Distilled from takthirdparty/ci-support/linux_cihost_prep.sh, minus the
# packages only the gdal/spatialite chains need (cmake, swig, tcl, bison,
# byacc, flex, p7zip, jq, patchelf) and minus git-lfs, which fetch-distfiles.sh
# replaces. `perl` is here for OpenSSL's Configure (IPC::Cmd) and `dos2unix` is
# invoked directly by the curl/libxml2/protobuf install recipes.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      autoconf \
      automake \
      build-essential \
      bzip2 \
      ca-certificates \
      curl \
      dos2unix \
      git \
      libtool \
      make \
      patch \
      perl \
      pkg-config \
      xz-utils \
 && rm -rf /var/lib/apt/lists/*

ARG ATAK_CIV_SHA
ENV ATAK_CIV_SHA=${ATAK_CIV_SHA}

# Each script is copied immediately before the step that runs it, so editing
# one only invalidates that step onward. Changing the pin invalidates the lot,
# which is the correct behaviour.

# Source acquisition: ~2 s / 18 MB for the trees, ~8 s / 37 MB for the
# distfiles, both measured. Two layers, so a distfile re-fetch does not
# re-clone.
COPY fetch-source.sh /usr/local/libexec/rustak-eud/
RUN /usr/local/libexec/rustak-eud/fetch-source.sh /src "${ATAK_CIV_SHA}"

COPY fetch-distfiles.sh /usr/local/libexec/rustak-eud/
RUN /usr/local/libexec/rustak-eud/fetch-distfiles.sh /src "${ATAK_CIV_SHA}"

# The expensive layer: ~25-45 min cold on a 4-vCPU runner, dominated by
# protobuf 3.21 (built twice — host protoc, then the target runtime) and
# OpenSSL 3.0.14-quic1. ghcr layer caching makes it a no-op when neither the
# pin nor this script moves.
#
# TTP_JOBS is the parallelism; empty means `nproc`. Set it to 1 (or something
# small) if a package in the chain turns out to misbehave under -j on a bump.
ARG TTP_JOBS=
ENV TTP_JOBS=${TTP_JOBS}
COPY build-takthirdparty.sh /usr/local/libexec/rustak-eud/
RUN /usr/local/libexec/rustak-eud/build-takthirdparty.sh /src

COPY stage-runtime.sh /usr/local/libexec/rustak-eud/
RUN /usr/local/libexec/rustak-eud/stage-runtime.sh /src /rootfs


FROM debian:bookworm-slim AS runtime
ARG ATAK_CIV_SHA

# libstdc++6/libgcc-s1 for the C++ binary; ca-certificates for completeness
# only — takthirdparty configures curl --without-ca-bundle --without-ca-path,
# so commoncommo verifies servers against the truststore it is handed, exactly
# as ATAK does.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /rootfs/ /

ENV LD_LIBRARY_PATH=/opt/commoncommo/lib

LABEL org.opencontainers.image.title="rustak interop EUD (commoncommo commotest)" \
      org.opencontainers.image.description="ATAK's commoncommo console test client, built from upstream atak-civ source, for rustak's nightly EUD interop suite." \
      org.opencontainers.image.vendor="Sierra Softworks" \
      org.opencontainers.image.source="https://github.com/SierraSoftworks/rustak" \
      org.opencontainers.image.documentation="https://github.com/SierraSoftworks/rustak/blob/main/interop/eud/README.md" \
      org.opencontainers.image.licenses="GPL-3.0-only" \
      dev.rustak.upstream.source="https://github.com/TAK-Product-Center/atak-civ" \
      dev.rustak.upstream.revision="${ATAK_CIV_SHA}" \
      dev.rustak.upstream.corresponding-source="https://github.com/TAK-Product-Center/atak-civ/tree/${ATAK_CIV_SHA}"

# commotest writes commo-log.txt, commo-xml.txt and (after enrolment)
# commo-enroll-cert.p12 into the output directory it is given on argv; mount a
# volume at /work and pass /work as that argument.
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/commotest"]
CMD ["-h"]
