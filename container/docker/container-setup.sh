#!/bin/sh
set -e
# Written into the container at run time by the compose entry point, so there
# is nothing for a static checker to read here.
# shellcheck source=/dev/null
. /tmp/afpay-env.sh

AFPAY_BASE="http://127.0.0.1:${AFPAY_REST_PORT}"
AUTH_HEADER="Authorization: Bearer ${AFPAY_REST_API_KEY_SECRET}"

# GET a domain resource.
afpay_get() {
    curl -s "${AFPAY_BASE}$1" -H "$AUTH_HEADER"
}

# POST a JSON body to a domain resource.
afpay_post() {
    curl -s -X POST "${AFPAY_BASE}$1" \
        -H "$AUTH_HEADER" \
        -H "Content-Type: application/json" \
        -d "$2"
}

# Readiness is the public discovery face: no credential, no domain state.
until curl -sf "${AFPAY_BASE}/health" 2>/dev/null | grep -q '"status":"ready"'; do
    sleep 1
done
echo "afpay HTTP API is ready"

# ── bitcoind wallet ──
if [ -n "$BTC_RPC_PASS" ]; then
    # Wait for bitcoind RPC
    until bitcoin-cli -rpcuser="$BTC_RPC_USER" -rpcpassword="$BTC_RPC_PASS" -rpcport="$BTC_RPC_PORT" getblockchaininfo 2>/dev/null; do
        sleep 2
    done
    # Create btc wallet if not exists
    EXISTING=$(afpay_get "/v1/wallets?network=btc" 2>/dev/null || true)
    if echo "$EXISTING" | grep -q '"network":"btc"'; then
        echo "btc wallet already exists, skipping"
    else
        afpay_post /v1/wallets "{\"network\":\"btc\",\"label\":\"btc-local\",\"backend\":\"core-rpc\",\"core_url\":\"http://127.0.0.1:${BTC_RPC_PORT}\",\"core_auth_secret\":\"${BTC_RPC_USER}:${BTC_RPC_PASS}\",\"btc_network\":\"${BTC_NETWORK:-mainnet}\"}"
        echo "btc wallet created"
    fi
fi

# ── phoenixd wallet ──
if [ "${ENABLE_PHOENIXD}" = "true" ]; then
    PW_FILE="${PHOENIXD_DATADIR}/.phoenix/http-password"
    PHOENIXD_CONF="${PHOENIXD_DATADIR}/.phoenix/phoenix.conf"
    PHOENIXD_PASS=""

    # Newer phoenixd versions store passwords in phoenix.conf, while older
    # builds may still persist a standalone http-password file.
    while [ -z "$PHOENIXD_PASS" ]; do
        if [ -f "$PW_FILE" ]; then
            PHOENIXD_PASS=$(cat "$PW_FILE")
        elif [ -f "$PHOENIXD_CONF" ]; then
            PHOENIXD_PASS=$(
                grep '^http-password=' "$PHOENIXD_CONF" | head -1 | cut -d= -f2-
            )
        fi
        if [ -z "$PHOENIXD_PASS" ]; then
            sleep 2
        fi
    done

    # Create ln wallet if not exists
    EXISTING=$(afpay_get "/v1/wallets?network=ln" 2>/dev/null || true)
    if echo "$EXISTING" | grep -q '"network":"ln"'; then
        echo "ln wallet already exists, skipping"
    else
        while :; do
            CREATE_RESPONSE=$(
                afpay_post /v1/wallets "{\"network\":\"ln\",\"backend\":\"phoenixd\",\"endpoint_url\":\"http://127.0.0.1:9740\",\"password_secret\":\"${PHOENIXD_PASS}\",\"label\":\"ln-local\"}" 2>/dev/null || true
            )
            if echo "$CREATE_RESPONSE" | grep -q '"kind":"result"'; then
                echo "ln-phoenixd wallet created"
                break
            fi
            if echo "$CREATE_RESPONSE" | grep -q '"code":"network_error"'; then
                sleep 2
                continue
            fi
            echo "$CREATE_RESPONSE"
            echo "ERROR: failed to create ln-phoenixd wallet" >&2
            exit 1
        done
    fi
fi

echo "setup complete"
