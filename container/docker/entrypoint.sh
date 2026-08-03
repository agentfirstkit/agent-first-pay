#!/bin/sh
set -e

AFPAY_MODE="${AFPAY_MODE:-rest}"
ENABLE_PHOENIXD="${ENABLE_PHOENIXD:-false}"
ENABLE_BITCOIND="${ENABLE_BITCOIND:-false}"
AFPAY_DATA_DIR="${AFPAY_DATA_DIR:-/data/afpay}"
BITCOIND_DATADIR="${BITCOIND_DATADIR:-/data/bitcoind}"
PHOENIXD_DATADIR="${PHOENIXD_DATADIR:-/data/phoenixd}"
AFPAY_PORT="${AFPAY_PORT:-9401}"

# Emit a TOML `key = ["a", "b"]` line from a comma-separated value list; emits
# nothing when the list is empty. Used to seed the operator allowlists below.
emit_allowlist() {
    key="$1"
    csv="$2"
    [ -n "$csv" ] || return 0
    arr=""
    OLD_IFS="$IFS"
    IFS=','
    for item in $csv; do
        [ -n "$item" ] || continue
        [ -n "$arr" ] && arr="$arr, "
        arr="$arr\"$item\""
    done
    IFS="$OLD_IFS"
    printf '%s = [%s]\n' "$key" "$arr"
}

mkdir -p "$AFPAY_DATA_DIR" "$BITCOIND_DATADIR" "$PHOENIXD_DATADIR"
chmod 700 "$AFPAY_DATA_DIR" "$BITCOIND_DATADIR" "$PHOENIXD_DATADIR" 2>/dev/null || true

# ── 0. Generate secret/key per mode, persist to file ──
case "$AFPAY_MODE" in
    rest)
        SECRET_FILE="${AFPAY_DATA_DIR}/rest-api-key-secret"
        LEGACY_SECRET_FILE="${AFPAY_DATA_DIR}/rest-api-key"
        SECRET_ENV="AFPAY_REST_API_KEY_SECRET"
        SECRET_VAL="${AFPAY_REST_API_KEY_SECRET:-${AFPAY_REST_API_KEY:-}}"
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
    if [ ! -f "$SECRET_FILE" ] && [ -n "${LEGACY_SECRET_FILE:-}" ] && [ -f "$LEGACY_SECRET_FILE" ]; then
        cp "$LEGACY_SECRET_FILE" "$SECRET_FILE"
    fi
    if [ -n "$SECRET_VAL" ]; then
        echo "$SECRET_VAL" > "$SECRET_FILE"
    elif [ -f "$SECRET_FILE" ]; then
        SECRET_VAL=$(cat "$SECRET_FILE")
    else
        SECRET_VAL="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
        echo "$SECRET_VAL" > "$SECRET_FILE"
    fi
    chmod 600 "$SECRET_FILE" 2>/dev/null || true
    export "$SECRET_ENV"="$SECRET_VAL"
fi

# ── 0b. Generate supervisor afpay.conf based on mode ──
case "$AFPAY_MODE" in
    rest)
        AFPAY_CMD="afpay --mode rest --public-listen --rest-listen 0.0.0.0:${AFPAY_PORT} --data-dir ${AFPAY_DATA_DIR}"
        echo "========================================="
        echo "  afpay mode: rest"
        echo "  afpay endpoint: 0.0.0.0:${AFPAY_PORT}"
        echo "  afpay API key:  configured (stored at ${SECRET_FILE})"
        echo ""
        echo "  curl -X POST http://localhost:${AFPAY_PORT}/v1/afpay \\"
        echo "    -H \"Authorization: Bearer \$(cat ${SECRET_FILE})\" \\"
        echo "    -H 'Content-Type: application/json' \\"
        echo "    -d '{\"code\":\"version\"}'"
        echo "========================================="
        ;;
    rpc)
        AFPAY_CMD="afpay --mode rpc --public-listen --rpc-listen 0.0.0.0:${AFPAY_PORT} --data-dir ${AFPAY_DATA_DIR}"
        echo "========================================="
        echo "  afpay mode: rpc"
        echo "  afpay endpoint: 0.0.0.0:${AFPAY_PORT}"
        echo "  afpay RPC secret: configured (stored at ${SECRET_FILE})"
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
    BTC_RPC_PORT="${BTC_RPC_PORT:-8332}"
    case "$BTC_NETWORK" in
        mainnet)
            BTC_NETWORK_CONFIG=""
            BTC_NETWORK_SECTION="main"
            ;;
        signet)
            BTC_NETWORK_CONFIG="signet=1"
            BTC_NETWORK_SECTION="signet"
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
    # The RPC block is network-scoped on purpose. bitcoind treats rpcbind and
    # rpcallowip as per-chain settings and, on any chain but mainnet, refuses to
    # start rather than ignore them: "Config setting for -rpcbind only applied on
    # signet network when in [signet] section." rpcport is pinned in the same
    # section because each chain otherwise picks its own default (8332 mainnet,
    # 38332 signet), while BTC_RPC_PORT is the single port container-setup.sh and
    # the wallet's btc_core_url talk to.
    cat > "${BITCOIND_DATADIR}/bitcoin.conf" <<EOF
server=1
${BTC_NETWORK_CONFIG}
${BTC_PRUNE_CONFIG}
[${BTC_NETWORK_SECTION}]
rpcuser=${BTC_RPC_USER}
rpcpassword=${BTC_RPC_PASS}
rpcbind=127.0.0.1
rpcallowip=127.0.0.1/32
rpcport=${BTC_RPC_PORT}
EOF
    # bitcoin.conf carries rpcpassword; same posture as the afpay secret file.
    chmod 600 "${BITCOIND_DATADIR}/bitcoin.conf" 2>/dev/null || true
else
    rm -f /etc/supervisor/conf.d/bitcoind.conf
fi

# ── 2. phoenixd: password file auto-generated on first start ──
if [ "$ENABLE_PHOENIXD" != "true" ]; then
    rm -f /etc/supervisor/conf.d/phoenixd.conf
fi

# ── 3. generate afpay config.toml (only if not already present) ──
# Operator allowlists (allowed_* arrays) come from the AFPAY_ALLOWED_* env, set by
# `afpay container install --allow <category>=<url>`. afpay refuses to start a
# public listener with an empty allowlist, so this is where the agent-unreachable
# boundary is seeded. config.toml is written once per data volume; to change the
# allowlists later, edit it or recreate the volume.
CONFIG_FILE="${AFPAY_DATA_DIR}/config.toml"
if [ ! -f "$CONFIG_FILE" ]; then
    {
        echo 'storage_backend = "redb"'
        emit_allowlist allowed_mint_urls "${AFPAY_ALLOWED_MINT_URLS:-}"
        emit_allowlist allowed_esplora_urls "${AFPAY_ALLOWED_ESPLORA_URLS:-}"
        emit_allowlist allowed_sol_rpc_endpoints "${AFPAY_ALLOWED_SOL_RPC_ENDPOINTS:-}"
        emit_allowlist allowed_evm_rpc_endpoints "${AFPAY_ALLOWED_EVM_RPC_ENDPOINTS:-}"
        emit_allowlist allowed_btc_core_urls "${AFPAY_ALLOWED_BTC_CORE_URLS:-}"
        emit_allowlist allowed_btc_electrum_urls "${AFPAY_ALLOWED_BTC_ELECTRUM_URLS:-}"
        emit_allowlist allowed_ln_endpoints "${AFPAY_ALLOWED_LN_ENDPOINTS:-}"
    } > "$CONFIG_FILE"
fi

# ── 4. write env file for container-setup.sh ──
cat > /tmp/afpay-env.sh <<EOF
AFPAY_DATA_DIR=${AFPAY_DATA_DIR}
AFPAY_REST_PORT=${AFPAY_PORT}
AFPAY_REST_API_KEY_SECRET=${SECRET_VAL}
EOF
chmod 600 /tmp/afpay-env.sh 2>/dev/null || true

if [ "$ENABLE_BITCOIND" = "true" ]; then
    cat >> /tmp/afpay-env.sh <<EOF
BTC_NETWORK=${BTC_NETWORK}
BTC_RPC_USER=${BTC_RPC_USER}
BTC_RPC_PASS=${BTC_RPC_PASS}
BTC_RPC_PORT=${BTC_RPC_PORT}
EOF
fi

# ── 5. start supervisord ──
exec supervisord -n -c /etc/supervisor/supervisord.conf
