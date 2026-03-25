#!/bin/sh
set -e

AFPAY_MODE="${AFPAY_MODE:-rest}"
ENABLE_PHOENIXD="${ENABLE_PHOENIXD:-false}"
ENABLE_BITCOIND="${ENABLE_BITCOIND:-false}"
AFPAY_DATA_DIR="${AFPAY_DATA_DIR:-/data/afpay}"
BITCOIND_DATADIR="${BITCOIND_DATADIR:-/data/bitcoind}"
PHOENIXD_DATADIR="${PHOENIXD_DATADIR:-/data/phoenixd}"
AFPAY_PORT="${AFPAY_PORT:-9401}"

mkdir -p "$AFPAY_DATA_DIR" "$BITCOIND_DATADIR" "$PHOENIXD_DATADIR"

# ── 0. Generate secret/key per mode, persist to file ──
case "$AFPAY_MODE" in
    rest)
        SECRET_FILE="${AFPAY_DATA_DIR}/rest-api-key"
        SECRET_ENV="AFPAY_REST_API_KEY"
        SECRET_VAL="${AFPAY_REST_API_KEY}"
        SECRET_LABEL="REST API key"
        ;;
    rpc)
        SECRET_FILE="${AFPAY_DATA_DIR}/rpc-secret"
        SECRET_ENV="AFPAY_RPC_SECRET"
        SECRET_VAL="${AFPAY_RPC_SECRET}"
        SECRET_LABEL="RPC secret"
        ;;
    *)
        echo "ERROR: unsupported AFPAY_MODE=${AFPAY_MODE} (expected: rest, rpc)"
        exit 1
        ;;
esac

if [ -n "$SECRET_FILE" ]; then
    if [ -n "$SECRET_VAL" ]; then
        echo "$SECRET_VAL" > "$SECRET_FILE"
    elif [ -f "$SECRET_FILE" ]; then
        SECRET_VAL=$(cat "$SECRET_FILE")
    else
        SECRET_VAL="$(head -c 32 /dev/urandom | base64 | tr -d '/+=' | head -c 32)"
        echo "$SECRET_VAL" > "$SECRET_FILE"
    fi
    export "$SECRET_ENV"="$SECRET_VAL"
fi

# ── 0b. Generate supervisor afpay.conf based on mode ──
case "$AFPAY_MODE" in
    rest)
        AFPAY_CMD="afpay --mode rest --rest-listen 0.0.0.0:${AFPAY_PORT} --rest-api-key ${SECRET_VAL} --data-dir ${AFPAY_DATA_DIR}"
        echo "========================================="
        echo "  afpay mode: rest"
        echo "  afpay endpoint: 0.0.0.0:${AFPAY_PORT}"
        echo "  afpay API key:  ${SECRET_VAL}"
        echo ""
        echo "  curl -X POST http://localhost:${AFPAY_PORT}/v1/afpay \\"
        echo "    -H 'Authorization: Bearer ${SECRET_VAL}' \\"
        echo "    -H 'Content-Type: application/json' \\"
        echo "    -d '{\"code\":\"version\"}'"
        echo "========================================="
        ;;
    rpc)
        AFPAY_CMD="afpay --mode rpc --rpc-listen 0.0.0.0:${AFPAY_PORT} --rpc-secret ${SECRET_VAL} --data-dir ${AFPAY_DATA_DIR}"
        echo "========================================="
        echo "  afpay mode: rpc"
        echo "  afpay endpoint: 0.0.0.0:${AFPAY_PORT}"
        echo "  afpay RPC secret: ${SECRET_VAL}"
        echo "========================================="
        ;;
esac

cat > /etc/supervisor/conf.d/afpay.conf <<EOF
[program:afpay]
command=${AFPAY_CMD}
autostart=true
autorestart=true
priority=20
stdout_logfile=/dev/stdout
stdout_logfile_maxbytes=0
stderr_logfile=/dev/stderr
stderr_logfile_maxbytes=0
EOF

# ── 0c. Setup script only works with REST mode (uses curl) ──
if [ "$AFPAY_MODE" != "rest" ]; then
    rm -f /etc/supervisor/conf.d/afpay-setup.conf
fi

# ── 1. bitcoind: generate random RPC password, write bitcoin.conf ──
if [ "$ENABLE_BITCOIND" = "true" ]; then
    BTC_RPC_USER="afpay"
    BTC_RPC_PASS="$(head -c 32 /dev/urandom | base64 | tr -d '/+=' | head -c 32)"
    BTC_NETWORK="${BTC_NETWORK:-mainnet}"
    BTC_PRUNE_MB="${BTC_PRUNE_MB:-550}"
    case "$BTC_NETWORK" in
        mainnet)
            BTC_NETWORK_CONFIG=""
            ;;
        signet)
            BTC_NETWORK_CONFIG="signet=1"
            ;;
        *)
            echo "ERROR: unsupported BTC_NETWORK=${BTC_NETWORK} (expected: mainnet or signet)"
            exit 1
            ;;
    esac
    case "$BTC_PRUNE_MB" in
        ''|*[!0-9]*)
            echo "ERROR: BTC_PRUNE_MB must be a non-negative integer"
            exit 1
            ;;
    esac
    if [ "$BTC_PRUNE_MB" -gt 0 ]; then
        BTC_PRUNE_CONFIG="prune=${BTC_PRUNE_MB}"
    else
        BTC_PRUNE_CONFIG=""
    fi
    cat > "${BITCOIND_DATADIR}/bitcoin.conf" <<EOF
server=1
${BTC_NETWORK_CONFIG}
${BTC_PRUNE_CONFIG}
rpcuser=${BTC_RPC_USER}
rpcpassword=${BTC_RPC_PASS}
rpcbind=127.0.0.1
rpcallowip=127.0.0.1/32
EOF
else
    rm -f /etc/supervisor/conf.d/bitcoind.conf
fi

# ── 2. phoenixd: password file auto-generated on first start ──
if [ "$ENABLE_PHOENIXD" != "true" ]; then
    rm -f /etc/supervisor/conf.d/phoenixd.conf
fi

# ── 3. generate afpay config.toml (only if not already present) ──
CONFIG_FILE="${AFPAY_DATA_DIR}/config.toml"
if [ ! -f "$CONFIG_FILE" ]; then
    cat > "$CONFIG_FILE" <<EOF
storage_backend = "redb"
EOF
fi

# ── 4. write env file for container-setup.sh ──
cat > /tmp/afpay-env.sh <<EOF
AFPAY_DATA_DIR=${AFPAY_DATA_DIR}
AFPAY_REST_PORT=${AFPAY_PORT}
AFPAY_REST_API_KEY=${SECRET_VAL}
EOF

if [ "$ENABLE_BITCOIND" = "true" ]; then
    cat >> /tmp/afpay-env.sh <<EOF
BTC_NETWORK=${BTC_NETWORK}
BTC_RPC_USER=${BTC_RPC_USER}
BTC_RPC_PASS=${BTC_RPC_PASS}
BTC_RPC_PORT=${BTC_RPC_PORT:-8332}
EOF
fi

# ── 5. start supervisord ──
exec supervisord -n -c /etc/supervisor/supervisord.conf
