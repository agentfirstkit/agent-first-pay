#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RUNTIME="${CONTAINER_RUNTIME:-docker}"
HELPER_IMAGE="${CONTAINER_HELPER_IMAGE:-busybox:1.36}"
STAMP=$(date -u +"%Y%m%dT%H%M%SZ")
BACKUP_PATH="${1:-$SCRIPT_DIR/backups/afpay-docker-backup-${STAMP}.tar.gz}"
INCLUDE_BITCOIND="${INCLUDE_BITCOIND:-false}"
AFPAY_VOLUME="${AFPAY_VOLUME:-afpay-data}"
PHOENIXD_VOLUME="${PHOENIXD_VOLUME:-phoenixd-data}"
BITCOIND_VOLUME="${BITCOIND_VOLUME:-bitcoind-data}"

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

TMP_PARENT="${CONTAINER_BACKUP_TMPDIR:-$SCRIPT_DIR/backups/.tmp}"
mkdir -p "$TMP_PARENT"
TMP_DIR=$(mktemp -d "${TMP_PARENT%/}/afpay-docker-backup.XXXXXX")
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

BUNDLE_DIR="$TMP_DIR/bundle"
mkdir -p "$BUNDLE_DIR" "$(dirname "$BACKUP_PATH")"

cat > "$BUNDLE_DIR/manifest.env" <<EOF
BACKUP_KIND=docker-volume
CREATED_AT_UTC=${STAMP}
RUNTIME=${RUNTIME}
HELPER_IMAGE=${HELPER_IMAGE}
AFPAY_VOLUME=${AFPAY_VOLUME}
PHOENIXD_VOLUME=${PHOENIXD_VOLUME}
BITCOIND_VOLUME=${BITCOIND_VOLUME}
INCLUDE_BITCOIND=${INCLUDE_BITCOIND}
EOF

archive_volume() {
    archive_name=$1
    volume_name=$2

    if ! "$RUNTIME" volume inspect "$volume_name" >/dev/null 2>&1; then
        return 0
    fi

    if ! "$RUNTIME" run --rm \
        -v "${volume_name}:/source:ro" \
        "$HELPER_IMAGE" \
        sh -eu -c "find /source -mindepth 1 -maxdepth 1 -print -quit | grep -q ." \
        >/dev/null 2>&1; then
        return 0
    fi

    "$RUNTIME" run --rm \
        -v "${volume_name}:/source:ro" \
        "$HELPER_IMAGE" \
        tar -C /source -czf - . > "$BUNDLE_DIR/${archive_name}.tar.gz"
}

archive_volume afpay "$AFPAY_VOLUME"
archive_volume phoenixd "$PHOENIXD_VOLUME"

if [ "$INCLUDE_BITCOIND" = "true" ]; then
    archive_volume bitcoind "$BITCOIND_VOLUME"
fi

tar -C "$BUNDLE_DIR" -czf "$BACKUP_PATH" .
printf 'Backup written to %s\n' "$BACKUP_PATH"
