#!/bin/sh
set -e
. /tmp/afpay-env.sh

AFPAY_URL="http://127.0.0.1:${AFPAY_REST_PORT}/v1/afpay"
AUTH_HEADER="Authorization: Bearer ${AFPAY_REST_API_KEY}"

# Helper: call afpay REST API
afpay_call() {
    curl -s -X POST "$AFPAY_URL" \
        -H "$AUTH_HEADER" \
        -H "Content-Type: application/json" \
        -d "$1"
}

# Wait for afpay REST to be ready
until afpay_call '{"code":"version"}' 2>/dev/null | grep -q '"version"'; do
    sleep 1
done
echo "afpay REST is ready"

# ── bitcoind wallet ──
if [ -n "$BTC_RPC_PASS" ]; then
    # Wait for bitcoind RPC
    until bitcoin-cli -rpcuser="$BTC_RPC_USER" -rpcpassword="$BTC_RPC_PASS" -rpcport="$BTC_RPC_PORT" getblockchaininfo 2>/dev/null; do
        sleep 2
    done
    # Create btc wallet if not exists
    EXISTING=$(afpay_call '{"code":"wallet_list","id":"setup_btc","network":"btc"}' 2>/dev/null || true)
    if echo "$EXISTING" | grep -q '"network":"btc"'; then
        echo "btc wallet already exists, skipping"
    else
        afpay_call "{\"code\":\"wallet_create\",\"id\":\"setup_btc_create\",\"network\":\"btc\",\"label\":\"btc-local\",\"btc_backend\":\"core-rpc\",\"btc_core_url\":\"http://127.0.0.1:${BTC_RPC_PORT}\",\"btc_core_auth_secret\":\"${BTC_RPC_USER}:${BTC_RPC_PASS}\",\"btc_network\":\"${BTC_NETWORK:-signet}\"}"
        echo "btc wallet created"
    fi
fi

# ── phoenixd wallet ──
if [ "${ENABLE_PHOENIXD}" = "true" ]; then
    # Wait for phoenixd to generate http-password
    PW_FILE="${PHOENIXD_DATADIR}/.phoenix/http-password"
    until [ -f "$PW_FILE" ]; do
        sleep 2
    done
    PHOENIXD_PASS=$(cat "$PW_FILE")
    # Create ln wallet if not exists
    EXISTING=$(afpay_call '{"code":"wallet_list","id":"setup_ln","network":"ln"}' 2>/dev/null || true)
    if echo "$EXISTING" | grep -q '"network":"ln"'; then
        echo "ln wallet already exists, skipping"
    else
        afpay_call "{\"code\":\"ln_wallet_create\",\"id\":\"setup_ln_create\",\"backend\":\"phoenixd\",\"endpoint\":\"http://127.0.0.1:9740\",\"password_secret\":\"${PHOENIXD_PASS}\",\"label\":\"ln-local\"}"
        echo "ln-phoenixd wallet created"
    fi
fi

echo "setup complete"
