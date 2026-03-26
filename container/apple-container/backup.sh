#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DATA_ROOT="${APPLE_CONTAINER_DATA_ROOT:-$SCRIPT_DIR/data}"
STAMP=$(date -u +"%Y%m%dT%H%M%SZ")
BACKUP_PATH="${1:-$SCRIPT_DIR/backups/afpay-apple-backup-${STAMP}.tar.zst}"
INCLUDE_BITCOIND="${INCLUDE_BITCOIND:-false}"

case "$INCLUDE_BITCOIND" in
    true|false) ;;
    *)
        echo "INCLUDE_BITCOIND must be true or false" >&2
        exit 1
        ;;
esac

mkdir -p "$(dirname "$BACKUP_PATH")"

EXTRA_ARGS=""
if [ -d "$DATA_ROOT/phoenixd" ]; then
    EXTRA_ARGS="$EXTRA_ARGS --extra-dir phoenixd=$DATA_ROOT/phoenixd"
fi
if [ "$INCLUDE_BITCOIND" = "true" ] && [ -d "$DATA_ROOT/bitcoind" ]; then
    EXTRA_ARGS="$EXTRA_ARGS --extra-dir bitcoind=$DATA_ROOT/bitcoind"
fi

# shellcheck disable=SC2086
afpay global backup \
    --data-dir "$DATA_ROOT/afpay" \
    --output "$BACKUP_PATH" \
    $EXTRA_ARGS

printf 'Backup written to %s\n' "$BACKUP_PATH"
