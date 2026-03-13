#!/bin/sh
set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 /path/to/backup.tar.gz" >&2
    exit 1
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RUNTIME="${CONTAINER_RUNTIME:-docker}"
HELPER_IMAGE="${CONTAINER_HELPER_IMAGE:-busybox:1.36}"
ARCHIVE_PATH=$1
AFPAY_VOLUME="${AFPAY_VOLUME:-afpay-data}"
PHOENIXD_VOLUME="${PHOENIXD_VOLUME:-phoenixd-data}"
BITCOIND_VOLUME="${BITCOIND_VOLUME:-bitcoind-data}"

if ! command -v "$RUNTIME" >/dev/null 2>&1; then
    echo "container runtime not found: $RUNTIME" >&2
    exit 1
fi
if [ ! -f "$ARCHIVE_PATH" ]; then
    echo "backup archive not found: $ARCHIVE_PATH" >&2
    exit 1
fi

TMP_PARENT="${CONTAINER_BACKUP_TMPDIR:-$SCRIPT_DIR/backups/.tmp}"
mkdir -p "$TMP_PARENT"
TMP_DIR=$(mktemp -d "${TMP_PARENT%/}/afpay-docker-restore.XXXXXX")
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

tar -C "$TMP_DIR" -xzf "$ARCHIVE_PATH"

restore_volume() {
    archive_name=$1
    volume_name=$2
    archive_file="$TMP_DIR/${archive_name}.tar.gz"

    if [ ! -f "$archive_file" ]; then
        return 0
    fi

    "$RUNTIME" volume create "$volume_name" >/dev/null
    "$RUNTIME" run --rm -i \
        -v "${volume_name}:/target" \
        "$HELPER_IMAGE" \
        sh -eu -c "
            find /target -mindepth 1 -maxdepth 1 -exec rm -rf {} \;
            tar -C /target -xzf -
        " < "$archive_file"
}

restore_volume afpay "$AFPAY_VOLUME"
restore_volume phoenixd "$PHOENIXD_VOLUME"
restore_volume bitcoind "$BITCOIND_VOLUME"

printf 'Restore completed from %s\n' "$ARCHIVE_PATH"
