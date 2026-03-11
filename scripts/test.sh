#!/bin/bash
set -euo pipefail

ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIER="${1:-all}"

# Keep test artifacts out of the repo/workspace.
AFPAY_TEST_HOME="$(mktemp -d "${TMPDIR:-/tmp}/afpay-test-XXXXXX")"
cleanup() {
  rm -rf "$AFPAY_TEST_HOME"
}
trap cleanup EXIT
export AFPAY_HOME="$AFPAY_TEST_HOME"

run_static() {
  echo "[static] fmt/build/clippy"
  (cd "$ROOTPATH" && cargo fmt --all --check)
  (cd "$ROOTPATH" && cargo build)
  (cd "$ROOTPATH" && cargo clippy -- -D warnings)
}

run_unit() {
  echo "[unit] Rust tests"
  # Exclude live tests that require network/wallet access
  (cd "$ROOTPATH" && cargo test --lib --bins)
}

case "$TIER" in
  static)
    run_static
    ;;
  unit)
    run_unit
    ;;
  all)
    run_static
    run_unit
    ;;
  *)
    echo "Usage: $0 [static|unit|all]" >&2
    exit 2
    ;;
esac

echo "Tier '$TIER' passed."
