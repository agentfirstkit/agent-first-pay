#!/bin/bash

cmn_test_usage() {
  echo "Usage: $(cmn_script_invocation) [static|unit|all]" >&2
}

cmn_test_prepare() {
  AFPAY_TEST_HOME="$(mktemp -d "${TMPDIR:-/tmp}/afpay-test-XXXXXX")"
  export AFPAY_TEST_HOME
  export AFPAY_HOME="$AFPAY_TEST_HOME"
}

cmn_test_cleanup() {
  if [ -n "${AFPAY_TEST_HOME:-}" ] && [ -d "$AFPAY_TEST_HOME" ]; then
    rm -rf "$AFPAY_TEST_HOME"
  fi
}

cmn_test_run_static() {
  echo "[static] fmt/build/clippy"
  cmn_cargo fmt --all --check
  cmn_cargo build
  cmn_cargo clippy -- -D warnings
}

cmn_test_run_unit() {
  echo "[unit] Rust tests"
  cmn_cargo test --lib --bins
}

cmn_test_run_all() {
  cmn_test_run_static
  cmn_test_run_unit
}
