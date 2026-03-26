#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RUNTIME="${CONTAINER_RUNTIME:-docker}"
COMPOSE="${COMPOSE_CMD:-$RUNTIME compose}"
STAMP=$(date -u +"%Y%m%dT%H%M%SZ")
BACKUP_PATH="${1:-$SCRIPT_DIR/backups/afpay-docker-backup-${STAMP}.tar.zst}"
INCLUDE_BITCOIND="${INCLUDE_BITCOIND:-false}"
SERVICE_NAME="${SERVICE_NAME:-afpay}"

case "$INCLUDE_BITCOIND" in
    true|false) ;;
    *)
        echo "INCLUDE_BITCOIND must be true or false" >&2
        exit 1
        ;;
esac

if ! command -v "$RUNTIME" >/dev/null 2>&1; then
    echo "container runtime not found: $RUNTIME" >&2
    exit 1
fi

mkdir -p "$(dirname "$BACKUP_PATH")"

# Resolve the running container name
CONTAINER=$($COMPOSE -f "$SCRIPT_DIR/compose.yaml" ps -q "$SERVICE_NAME" 2>/dev/null)
if [ -z "$CONTAINER" ]; then
    echo "container '$SERVICE_NAME' is not running; start it first with ./up.sh" >&2
    exit 1
fi

# Build afpay global backup command with extra dirs for phoenixd/bitcoind
EXTRA_ARGS=""
if "$RUNTIME" exec "$CONTAINER" test -d /data/phoenixd 2>/dev/null; then
    EXTRA_ARGS="$EXTRA_ARGS --extra-dir phoenixd=/data/phoenixd"
fi
if [ "$INCLUDE_BITCOIND" = "true" ]; then
    if "$RUNTIME" exec "$CONTAINER" test -d /data/bitcoind 2>/dev/null; then
        EXTRA_ARGS="$EXTRA_ARGS --extra-dir bitcoind=/data/bitcoind"
    fi
fi

# Run backup inside the container, stream archive to host
# shellcheck disable=SC2086
"$RUNTIME" exec "$CONTAINER" \
    afpay global backup --output /tmp/afpay-backup.tar.zst $EXTRA_ARGS

# Copy archive from container to host
"$RUNTIME" cp "$CONTAINER:/tmp/afpay-backup.tar.zst" "$BACKUP_PATH"
"$RUNTIME" exec "$CONTAINER" rm -f /tmp/afpay-backup.tar.zst

printf 'Backup written to %s\n' "$BACKUP_PATH"
