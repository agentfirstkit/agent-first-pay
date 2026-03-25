#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DATA_ROOT="${APPLE_CONTAINER_DATA_ROOT:-$SCRIPT_DIR/data}"
STAMP=$(date -u +"%Y%m%dT%H%M%SZ")
BACKUP_PATH="${1:-$SCRIPT_DIR/backups/afpay-apple-backup-${STAMP}.tar.gz}"
INCLUDE_BITCOIND="${INCLUDE_BITCOIND:-false}"

case "$INCLUDE_BITCOIND" in
    true|false) ;;
    *)
        echo "INCLUDE_BITCOIND must be true or false" >&2
        exit 1
        ;;
esac

TMP_DIR=$(mktemp -d)
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

BUNDLE_DIR="$TMP_DIR/bundle"
mkdir -p "$BUNDLE_DIR" "$(dirname "$BACKUP_PATH")"

cat > "$BUNDLE_DIR/manifest.env" <<EOF
BACKUP_KIND=apple-container
CREATED_AT_UTC=${STAMP}
DATA_ROOT=${DATA_ROOT}
INCLUDE_BITCOIND=${INCLUDE_BITCOIND}
EOF

archive_dir() {
    archive_name=$1
    source_dir=$2

    if [ ! -d "$source_dir" ]; then
        return 0
    fi
    if ! find "$source_dir" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; then
        return 0
    fi

    tar -C "$source_dir" -czf "$BUNDLE_DIR/${archive_name}.tar.gz" .
}

archive_dir afpay "$DATA_ROOT/afpay"
archive_dir phoenixd "$DATA_ROOT/phoenixd"

if [ "$INCLUDE_BITCOIND" = "true" ]; then
    archive_dir bitcoind "$DATA_ROOT/bitcoind"
fi

tar -C "$BUNDLE_DIR" -czf "$BACKUP_PATH" .
printf 'Backup written to %s\n' "$BACKUP_PATH"
