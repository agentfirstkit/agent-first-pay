#!/usr/bin/env bash
# Fails when the third-party versions the container image installs drift behind
# upstream latest. Both pins sat three (bitcoind 28.1 → 31.1) and two (phoenixd
# 0.7.2 → 0.9.0) releases behind before anything noticed, because nothing in the
# repository ever compared them against upstream.
#
# Drift is a hard failure. An unreachable upstream is not: it is evidence of
# nothing, least of all that a pin is stale. Every fetch retries with backoff,
# and only once the retries are exhausted does the check warn loudly and skip
# that one comparison.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DOCKERFILE="$SPORE_DIR/container/docker/Dockerfile"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "missing required command: $1" >&2
        exit 2
    }
}

# Both pins are declared as `ARG NAME=version` so they are readable from here
# without parsing the install commands that interpolate them.
pin() {
    sed -n "s/^ARG $1=\\(.*\\)$/\\1/p" "$DOCKERFILE"
}

require_pin() {
    local name="$1" value
    value="$(pin "$name")"
    if [ -z "$value" ]; then
        echo "missing pin ARG $name in $DOCKERFILE" >&2
        exit 2
    fi
    printf '%s\n' "$value"
}

expect_eq() {
    local label="$1" current="$2" latest="$3"
    if [ "$current" != "$latest" ]; then
        echo "outdated: $label current=$current latest=$latest" >&2
        return 1
    fi
    echo "ok: $label $current"
}

# Same retry budget as the crates.io query in scripts/release/lib.sh: every
# error class is retryable because none of them distinguish a stale pin.
curl_retry=(--retry 5 --retry-all-errors --retry-delay 2 --retry-max-time 60)

# Echoes the response body, or warns and returns 1 when the endpoint stays
# unreachable. Callers must set unreachable=1 themselves — this runs inside a
# command substitution, so assignments here would be lost with the subshell.
fetch_json() {
    local label="$1" url="$2" body
    if ! body="$(curl -fsSL "${curl_retry[@]}" "$url")" || [ -z "$body" ]; then
        echo "unreachable: $label <$url> did not answer after retries" >&2
        return 1
    fi
    printf '%s' "$body"
}

# A 200 carrying no version — rate-limit JSON, a reshaped API, a truncated body
# — is not drift evidence either. Comparing the pin against "null" would report
# a drift that upstream never claimed.
usable() {
    local label="$1" value="$2"
    if [ -z "$value" ] || [ "$value" = "null" ]; then
        echo "unreadable: $label answered without a usable version" >&2
        return 1
    fi
}

need curl
need jq

fail=0
unreachable=0

# ── phoenixd: the supervisord-managed Lightning backend ──
if phoenixd_json="$(fetch_json "phoenixd" "https://api.github.com/repos/ACINQ/phoenixd/releases/latest")"; then
    latest_phoenixd="$(printf '%s' "$phoenixd_json" | jq -r '.tag_name | sub("^v"; "")')" || latest_phoenixd=""
    if usable "phoenixd" "$latest_phoenixd"; then
        # The image resolves one of these two by container arch, so a release
        # that dropped either would break the build on that arch alone.
        for phoenixd_arch in linux-x64 linux-arm64; do
            phoenixd_asset="phoenixd-${latest_phoenixd}-${phoenixd_arch}.zip"
            if ! printf '%s' "$phoenixd_json" | jq -e --arg name "$phoenixd_asset" 'any(.assets[].name; . == $name)' >/dev/null; then
                echo "missing expected phoenixd asset: $phoenixd_asset" >&2
                fail=1
            fi
        done
        expect_eq "phoenixd" "$(require_pin PHOENIXD_VERSION)" "$latest_phoenixd" || fail=1
    else
        unreachable=1
    fi
else
    unreachable=1
fi

# ── bitcoind: Bitcoin Core, for the btc-core RPC chain source ──
# GitHub carries the authoritative version; the binaries come from bitcoincore.org.
if bitcoind_json="$(fetch_json "bitcoind" "https://api.github.com/repos/bitcoin/bitcoin/releases/latest")"; then
    latest_bitcoind="$(printf '%s' "$bitcoind_json" | jq -r '.tag_name | sub("^v"; "")')" || latest_bitcoind=""
    if usable "bitcoind" "$latest_bitcoind"; then
        # bitcoincore.org publishes separately from the GitHub tag. A version
        # whose tarballs are not up yet cannot be adopted, so it is not drift to
        # report — it is a pin this run could not check.
        missing_tarball=0
        for bitcoind_arch in x86_64-linux-gnu aarch64-linux-gnu; do
            tarball="https://bitcoincore.org/bin/bitcoin-core-${latest_bitcoind}/bitcoin-${latest_bitcoind}-${bitcoind_arch}.tar.gz"
            if ! curl -fsIL "${curl_retry[@]}" -o /dev/null "$tarball"; then
                echo "unreachable: bitcoin-core ${latest_bitcoind} ${bitcoind_arch} tarball <$tarball> is not published" >&2
                missing_tarball=1
            fi
        done
        if [ "$missing_tarball" -ne 0 ]; then
            unreachable=1
        else
            expect_eq "bitcoind" "$(require_pin BITCOIND_VERSION)" "$latest_bitcoind" || fail=1
        fi
    else
        unreachable=1
    fi
else
    unreachable=1
fi

if [ "$fail" -ne 0 ]; then
    echo "Update the ARG pins in container/docker/Dockerfile before release." >&2
    echo "A bitcoind bump also has to re-verify the bitcoin.conf that entrypoint.sh writes:" >&2
    echo "rpcbind/rpcallowip/rpcport are network-scoped, and bitcoind fails to start when they are not." >&2
    exit 1
fi

if [ "$unreachable" -ne 0 ]; then
    echo "WARNING: some upstreams stayed unreachable after retries; those pins were NOT checked." >&2
    echo "WARNING: this run does not certify them current. Re-run on a working network before release." >&2
fi
