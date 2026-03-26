#!/bin/sh
set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 /path/to/backup.tar.zst" >&2
    exit 1
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DATA_ROOT="${APPLE_CONTAINER_DATA_ROOT:-$SCRIPT_DIR/data}"
ARCHIVE_PATH=$1

if [ ! -f "$ARCHIVE_PATH" ]; then
    echo "backup archive not found: $ARCHIVE_PATH" >&2
    exit 1
fi

EXTRA_ARGS=""
if [ -d "$DATA_ROOT/phoenixd" ]; then
    EXTRA_ARGS="$EXTRA_ARGS --extra-dir phoenixd=$DATA_ROOT/phoenixd"
fi
if [ -d "$DATA_ROOT/bitcoind" ]; then
    EXTRA_ARGS="$EXTRA_ARGS --extra-dir bitcoind=$DATA_ROOT/bitcoind"
fi

# shellcheck disable=SC2086
afpay global restore \
    --data-dir "$DATA_ROOT/afpay" \
    --dangerously-overwrite \
    "$ARCHIVE_PATH" \
    $EXTRA_ARGS

printf 'Restore completed from %s\n' "$ARCHIVE_PATH"
