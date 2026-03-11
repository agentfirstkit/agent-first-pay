# ── Build stage ──
FROM rust:1.86-bookworm AS builder

ARG FEATURES="btc-esplora,cashu,ln-nwc,redb,mcp,rest,exchange-rate"

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY tests/ tests/

RUN cargo build --release --no-default-features --features "${FEATURES}" \
    && cp target/release/afpay /usr/local/bin/afpay

# ── Runtime stage ──
FROM debian:bookworm-slim

ARG INSTALL_PHOENIXD=false
ARG INSTALL_BITCOIND=false

RUN apt-get update && apt-get install -y --no-install-recommends \
        supervisor ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/afpay /usr/local/bin/afpay

# Install phoenixd if requested
RUN if [ "$INSTALL_PHOENIXD" = "true" ]; then \
        ARCH=$(uname -m) && \
        if [ "$ARCH" = "x86_64" ]; then ARCH="linux-x64"; \
        elif [ "$ARCH" = "aarch64" ]; then ARCH="linux-arm64"; \
        else echo "unsupported arch: $ARCH" && exit 1; fi && \
        curl -fsSL "https://github.com/nicoletyy/phoenixd/releases/download/v0.5.1/phoenix-0.5.1-${ARCH}.zip" -o /tmp/phoenixd.zip && \
        apt-get update && apt-get install -y --no-install-recommends unzip && \
        unzip /tmp/phoenixd.zip -d /tmp/phoenixd && \
        cp /tmp/phoenixd/phoenix-*/phoenixd /usr/local/bin/ && \
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
        curl -fsSL "https://bitcoincore.org/bin/bitcoin-core-28.1/bitcoin-28.1-${ARCH}.tar.gz" -o /tmp/bitcoin.tar.gz && \
        tar -xzf /tmp/bitcoin.tar.gz -C /tmp && \
        cp /tmp/bitcoin-*/bin/bitcoind /usr/local/bin/ && \
        cp /tmp/bitcoin-*/bin/bitcoin-cli /usr/local/bin/ && \
        rm -rf /tmp/bitcoin* ; \
    fi

# Supervisor config
COPY docker/supervisord.conf /etc/supervisor/supervisord.conf
COPY docker/conf.d/ /etc/supervisor/conf.d/

# Entrypoint + setup script
COPY docker/entrypoint.sh /entrypoint.sh
COPY docker/docker-setup.sh /docker-setup.sh
RUN chmod +x /entrypoint.sh /docker-setup.sh

ENV AFPAY_MODE=rest \
    AFPAY_DATA_DIR=/data/afpay \
    BITCOIND_DATADIR=/data/bitcoind \
    PHOENIXD_DATADIR=/data/phoenixd \
    AFPAY_PORT=9401

EXPOSE 9401

ENTRYPOINT ["/entrypoint.sh"]
