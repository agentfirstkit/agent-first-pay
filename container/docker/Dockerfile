# Runtime image for the afpay daemon (supervisord runs afpay + optional bitcoind
# + phoenixd). See docs and container/README.md.
#
# The afpay binary comes from ONE of two source stages, picked by the
# AFPAY_BIN_FROM build-arg:
#   builder    — compile from a source checkout (default; dev / `--from-source`).
#                Build context is the spore root:
#                  docker build -t afpay -f container/docker/Dockerfile .
#   downloader — download the matching prebuilt release; what `afpay container
#                install` uses. Needs --build-arg AFPAY_VERSION + AFPAY_TARGET.
# BuildKit only builds the selected stage, so the download path pulls no Rust
# toolchain and needs no source tree, while the from-source path makes no network
# call for a release. (The prebuilt release ships afpay's default feature set — a
# superset of the from-source FEATURES below — so it is at least as capable.)

# Selects which source stage below provides the binary. Global (before the first
# FROM) so it is usable on a FROM line.
ARG AFPAY_BIN_FROM=builder

# ── Source stage A: compile from a checkout (default) ──
FROM rust:1-trixie AS builder

ARG FEATURES="btc-esplora,cashu,ln-phoenixd,redb,rest,exchange-rate,backup"

WORKDIR /src
# NOT .cargo/ (its dev path override to ../agent-first-data is absent here,
# so agent-first-data resolves from crates.io per Cargo.lock).
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ src/
COPY tests/ tests/
# Embedded at compile time via include_str!: the skill (skill_admin.rs) and the
# container build recipe + scripts (container.rs). Only container/docker/ — not
# all of container/ (apple-container/ holds local wallet state).
COPY skills/ skills/
COPY container/docker/ container/docker/

RUN cargo build --release --no-default-features --features "${FEATURES}" \
    && cp target/release/afpay /usr/local/bin/afpay

# ── Source stage B: download the matching prebuilt release ──
FROM debian:trixie-slim AS downloader
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# Hard-pinned by the driver: its own version + the linux triple for the image
# arch. A 404 here means no release asset exists for this version/arch — the
# driver surfaces that and points at the from-source path.
ARG AFPAY_VERSION
ARG AFPAY_TARGET
RUN set -eu; \
    base="https://github.com/agentfirstkit/agent-first-pay/releases/download/v${AFPAY_VERSION}"; \
    archive="afpay-v${AFPAY_VERSION}-${AFPAY_TARGET}.tar.gz"; \
    cd /tmp; \
    curl -fsSL -o "$archive" "${base}/${archive}"; \
    curl -fsSL -o "${archive}.sha256" "${base}/${archive}.sha256"; \
    sha256sum -c "${archive}.sha256"; \
    tar -xzf "$archive" -C /usr/local/bin afpay; \
    chmod +x /usr/local/bin/afpay

# ── Pick the source stage that provides /usr/local/bin/afpay ──
FROM ${AFPAY_BIN_FROM} AS afpay_bin

# ── Runtime stage ──
FROM debian:trixie-slim

ARG INSTALL_PHOENIXD=false
ARG INSTALL_BITCOIND=false
# Both pins are `ARG NAME=version` lines so tests/check_container_third_party_versions.sh
# can read them; keep that shape when adding a third-party install.
ARG PHOENIXD_VERSION=0.9.0
ARG BITCOIND_VERSION=31.1

RUN apt-get update && apt-get install -y --no-install-recommends \
        supervisor ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=afpay_bin /usr/local/bin/afpay /usr/local/bin/afpay

# Install phoenixd if requested
RUN if [ "$INSTALL_PHOENIXD" = "true" ]; then \
        ARCH=$(uname -m) && \
        if [ "$ARCH" = "x86_64" ]; then ARCH="linux-x64"; \
        elif [ "$ARCH" = "aarch64" ]; then ARCH="linux-arm64"; \
        else echo "unsupported arch: $ARCH" && exit 1; fi && \
        curl -fsSL "https://github.com/ACINQ/phoenixd/releases/download/v${PHOENIXD_VERSION}/phoenixd-${PHOENIXD_VERSION}-${ARCH}.zip" -o /tmp/phoenixd.zip && \
        apt-get update && apt-get install -y --no-install-recommends unzip && \
        unzip /tmp/phoenixd.zip -d /tmp/phoenixd && \
        cp /tmp/phoenixd/phoenixd-*/phoenixd /usr/local/bin/ && \
        chmod +x /usr/local/bin/phoenixd && \
        rm -rf /tmp/phoenixd /tmp/phoenixd.zip && \
        apt-get purge -y unzip && apt-get autoremove -y && rm -rf /var/lib/apt/lists/*; \
    fi

# Install bitcoind if requested
RUN if [ "$INSTALL_BITCOIND" = "true" ]; then \
        ARCH=$(uname -m) && \
        if [ "$ARCH" = "x86_64" ]; then ARCH="x86_64-linux-gnu"; \
        elif [ "$ARCH" = "aarch64" ]; then ARCH="aarch64-linux-gnu"; \
        else echo "unsupported arch: $ARCH" && exit 1; fi && \
        curl -fsSL "https://bitcoincore.org/bin/bitcoin-core-${BITCOIND_VERSION}/bitcoin-${BITCOIND_VERSION}-${ARCH}.tar.gz" -o /tmp/bitcoin.tar.gz && \
        tar -xzf /tmp/bitcoin.tar.gz -C /tmp && \
        cp /tmp/bitcoin-*/bin/bitcoind /usr/local/bin/ && \
        cp /tmp/bitcoin-*/bin/bitcoin-cli /usr/local/bin/ && \
        rm -rf /tmp/bitcoin* ; \
    fi

# Supervisor config
COPY container/docker/supervisord.conf /etc/supervisor/supervisord.conf
COPY container/docker/conf.d/ /etc/supervisor/conf.d/

# Entrypoint + setup script
COPY container/docker/entrypoint.sh /entrypoint.sh
COPY container/docker/container-setup.sh /container-setup.sh
RUN chmod +x /entrypoint.sh /container-setup.sh

ENV AFPAY_DATA_DIR=/data/afpay \
    BITCOIND_DATADIR=/data/bitcoind \
    PHOENIXD_DATADIR=/data/phoenixd \
    AFPAY_PORT=9401

EXPOSE 9401

ENTRYPOINT ["/entrypoint.sh"]
