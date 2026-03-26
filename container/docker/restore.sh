#!/bin/sh
set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 /path/to/backup.tar.zst" >&2
    exit 1
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RUNTIME="${CONTAINER_RUNTIME:-docker}"
COMPOSE="${COMPOSE_CMD:-$RUNTIME compose}"
ARCHIVE_PATH=$1
SERVICE_NAME="${SERVICE_NAME:-afpay}"

if ! command -v "$RUNTIME" >/dev/null 2>&1; then
    echo "container runtime not found: $RUNTIME" >&2
    exit 1
fi
if [ ! -f "$ARCHIVE_PATH" ]; then
    echo "backup archive not found: $ARCHIVE_PATH" >&2
    exit 1
fi

# Resolve the running container name
CONTAINER=$($COMPOSE -f "$SCRIPT_DIR/compose.yaml" ps -q "$SERVICE_NAME" 2>/dev/null)
if [ -z "$CONTAINER" ]; then
    echo "container '$SERVICE_NAME' is not running; start it first with ./up.sh" >&2
    exit 1
fi

# Copy archive into container
"$RUNTIME" cp "$ARCHIVE_PATH" "$CONTAINER:/tmp/afpay-restore.tar.zst"

# Build extra-dir args for phoenixd/bitcoind
EXTRA_ARGS=""
if "$RUNTIME" exec "$CONTAINER" test -d /data/phoenixd 2>/dev/null; then
    EXTRA_ARGS="$EXTRA_ARGS --extra-dir phoenixd=/data/phoenixd"
fi
if "$RUNTIME" exec "$CONTAINER" test -d /data/bitcoind 2>/dev/null; then
    EXTRA_ARGS="$EXTRA_ARGS --extra-dir bitcoind=/data/bitcoind"
fi

# Run restore inside the container
# shellcheck disable=SC2086
"$RUNTIME" exec "$CONTAINER" \
    afpay global restore --dangerously-overwrite /tmp/afpay-restore.tar.zst $EXTRA_ARGS

"$RUNTIME" exec "$CONTAINER" rm -f /tmp/afpay-restore.tar.zst

printf 'Restore completed from %s\n' "$ARCHIVE_PATH"
