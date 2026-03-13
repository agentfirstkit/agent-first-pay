#!/bin/sh
set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 /path/to/backup.tar.gz" >&2
    exit 1
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DATA_ROOT="${APPLE_CONTAINER_DATA_ROOT:-$SCRIPT_DIR/data}"
ARCHIVE_PATH=$1

if [ ! -f "$ARCHIVE_PATH" ]; then
    echo "backup archive not found: $ARCHIVE_PATH" >&2
    exit 1
fi

TMP_DIR=$(mktemp -d)
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

tar -C "$TMP_DIR" -xzf "$ARCHIVE_PATH"

restore_dir() {
    archive_name=$1
    target_dir=$2
    archive_file="$TMP_DIR/${archive_name}.tar.gz"

    if [ ! -f "$archive_file" ]; then
        return 0
    fi

    mkdir -p "$target_dir"
    find "$target_dir" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
    tar -C "$target_dir" -xzf "$archive_file"
}

restore_dir afpay "$DATA_ROOT/afpay"
restore_dir phoenixd "$DATA_ROOT/phoenixd"
restore_dir bitcoind "$DATA_ROOT/bitcoind"

printf 'Restore completed from %s\n' "$ARCHIVE_PATH"
