#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

find_repo_root() {
  local dir="$PROJECT_ROOT"

  while [ "$dir" != "/" ]; do
    if [ -f "$dir/scripts/rust-project/lib.sh" ]; then
      printf '%s\n' "$dir"
      return 0
    fi

    dir="$(dirname "$dir")"
  done

  return 1
}

# Try monorepo delegation first; fall back to standalone mode.
if REPO_ROOT="$(find_repo_root)" 2>/dev/null; then
  SCRIPT_NAME="$(basename "$0")"
  export CMN_RUST_PROJECT_ROOT="$PROJECT_ROOT"
  export CMN_RUST_SCRIPT_INVOCATION="$0"
  exec "$REPO_ROOT/scripts/rust-project/$SCRIPT_NAME" "$@"
fi

# --- Standalone mode (e.g. GitHub CI on the individual repo) ---

SCRIPT_NAME="$(basename "$0" .sh)"
MODE="${1:-all}"

cmn_cargo() { (cd "$PROJECT_ROOT" && cargo "$@"); }

load_hooks() {
  local hooks="$PROJECT_ROOT/scripts/test-hooks.sh"

  # Provide the helpers that test-hooks.sh expects
  cmn_project_root() { printf '%s\n' "$PROJECT_ROOT"; }
  cmn_script_invocation() { printf '%s\n' "$0"; }
  cmn_function_exists() { declare -F "$1" >/dev/null 2>&1; }

  if [ -f "$hooks" ]; then
    # shellcheck disable=SC1090
    . "$hooks"
  fi
}

run_test() {
  load_hooks

  local project_name
  project_name="$(sed -nE '/^\[package\]/,/^\[/{s/^name = "([^"]+)".*/\1/p;}' "$PROJECT_ROOT/Cargo.toml" | head -1)"
  : "${project_name:=$(basename "$PROJECT_ROOT")}"

  echo "Testing $project_name [$MODE]..."

  case "$MODE" in
    static)
      if cmn_function_exists cmn_test_run_static; then cmn_test_run_static
      else cmn_cargo fmt --all --check && cmn_cargo clippy --all-targets -- -D warnings; fi
      ;;
    unit)
      if cmn_function_exists cmn_test_run_unit; then cmn_test_run_unit
      else cmn_cargo test; fi
      ;;
    all)
      if cmn_function_exists cmn_test_run_all; then cmn_test_run_all
      elif cmn_function_exists cmn_test_run_static && cmn_function_exists cmn_test_run_unit; then
        cmn_test_run_static; cmn_test_run_unit
      else cmn_cargo fmt --all --check && cmn_cargo clippy --all-targets -- -D warnings && cmn_cargo test; fi
      ;;
    integration|e2e|coverage)
      local handler="cmn_test_run_${MODE}"
      if cmn_function_exists "$handler"; then "$handler"
      else echo "No $MODE hook defined in test-hooks.sh" >&2; exit 2; fi
      ;;
    *)
      echo "Usage: $0 [static|unit|integration|e2e|coverage|all]" >&2
      exit 2
      ;;
  esac

  echo "All checks passed for $project_name [$MODE]!"
}

run_format() {
  echo "Formatting..."
  cmn_cargo fmt --all
  echo "Format complete!"
}

case "$SCRIPT_NAME" in
  test)     run_test ;;
  format)   run_format ;;
  *)
    echo "Standalone mode does not support script: $SCRIPT_NAME" >&2
    exit 2
    ;;
esac
