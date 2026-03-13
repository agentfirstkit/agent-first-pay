#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
DATA_DIR="$SCRIPT_DIR/data"
IMAGE_NAME="${AFPAY_APPLE_IMAGE:-afpay:apple}"
CONTAINER_NAME="${AFPAY_APPLE_CONTAINER:-afpay-apple}"
BUILD_PLATFORM="${AFPAY_APPLE_BUILD_PLATFORM:-linux/arm64}"
AFPAY_MODE="${AFPAY_MODE:-rest}"
AFPAY_PORT="${AFPAY_PORT:-9401}"
AFPAY_HOST_IP="${AFPAY_HOST_IP:-127.0.0.1}"
ENABLE_PHOENIXD="${ENABLE_PHOENIXD:-true}"
ENABLE_BITCOIND="${ENABLE_BITCOIND:-false}"
BTC_NETWORK="${BTC_NETWORK:-mainnet}"
BTC_RPC_PORT="${BTC_RPC_PORT:-8332}"
BTC_PRUNE_MB="${BTC_PRUNE_MB:-550}"
FEATURES="${FEATURES:-btc-core,ln-phoenixd,cashu,redb,mcp,rest,exchange-rate}"
INSTALL_PHOENIXD="${INSTALL_PHOENIXD:-true}"
if [ "${INSTALL_BITCOIND+set}" = "set" ]; then
    INSTALL_BITCOIND="${INSTALL_BITCOIND}"
else
    INSTALL_BITCOIND="$ENABLE_BITCOIND"
fi
DETACH_MODE="${AFPAY_APPLE_DETACH:-auto}"

if ! command -v container >/dev/null 2>&1; then
    echo "Apple container CLI is required. Install it from https://github.com/apple/container/releases" >&2
    exit 1
fi

case "$DETACH_MODE" in
    auto)
        if [ "$AFPAY_MODE" = "mcp" ]; then
            DETACH_MODE=false
        else
            DETACH_MODE=true
        fi
        ;;
    true|false)
        ;;
    *)
        echo "AFPAY_APPLE_DETACH must be auto, true, or false" >&2
        exit 1
        ;;
esac

mkdir -p "$DATA_DIR/afpay" "$DATA_DIR/bitcoind" "$DATA_DIR/phoenixd"

container system start

container build \
    --platform "$BUILD_PLATFORM" \
    --build-arg "FEATURES=$FEATURES" \
    --build-arg "INSTALL_PHOENIXD=$INSTALL_PHOENIXD" \
    --build-arg "INSTALL_BITCOIND=$INSTALL_BITCOIND" \
    -t "$IMAGE_NAME" \
    -f "$PROJECT_DIR/container/docker/Dockerfile" \
    "$PROJECT_DIR"

container stop "$CONTAINER_NAME" >/dev/null 2>&1 || true
container delete "$CONTAINER_NAME" >/dev/null 2>&1 || true

if [ "$DETACH_MODE" = "true" ]; then
    set -- container run -d
else
    set -- container run
fi

set -- "$@" --name "$CONTAINER_NAME" \
    -v "$DATA_DIR/afpay:/data/afpay" \
    -v "$DATA_DIR/bitcoind:/data/bitcoind" \
    -v "$DATA_DIR/phoenixd:/data/phoenixd" \
    -e "AFPAY_MODE=$AFPAY_MODE" \
    -e "AFPAY_PORT=$AFPAY_PORT" \
    -e "ENABLE_PHOENIXD=$ENABLE_PHOENIXD" \
    -e "ENABLE_BITCOIND=$ENABLE_BITCOIND" \
    -e "BTC_NETWORK=$BTC_NETWORK" \
    -e "BTC_RPC_PORT=$BTC_RPC_PORT" \
    -e "BTC_PRUNE_MB=$BTC_PRUNE_MB"

if [ -n "${AFPAY_REST_API_KEY:-}" ]; then
    set -- "$@" -e "AFPAY_REST_API_KEY=$AFPAY_REST_API_KEY"
fi

if [ -n "${AFPAY_RPC_SECRET:-}" ]; then
    set -- "$@" -e "AFPAY_RPC_SECRET=$AFPAY_RPC_SECRET"
fi

if [ "$AFPAY_MODE" != "mcp" ]; then
    set -- "$@" -p "${AFPAY_HOST_IP}:${AFPAY_PORT}:${AFPAY_PORT}"
fi

set -- "$@" "$IMAGE_NAME"

printf 'Starting %s with Apple container CLI using image %s\n' "$CONTAINER_NAME" "$IMAGE_NAME"
printf 'Build platform: %s\n' "$BUILD_PLATFORM"
if [ "$AFPAY_MODE" != "mcp" ]; then
    printf 'Listening on http://%s:%s\n' "$AFPAY_HOST_IP" "$AFPAY_PORT"
fi

exec "$@"
