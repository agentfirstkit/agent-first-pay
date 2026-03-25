#!/bin/sh
set -eu

CONTAINER_NAME="${AFPAY_APPLE_CONTAINER:-afpay-apple}"

if ! command -v container >/dev/null 2>&1; then
    echo "Apple container CLI is required. Install it from https://github.com/apple/container/releases" >&2
    exit 1
fi

exec container logs "$@" "$CONTAINER_NAME"
