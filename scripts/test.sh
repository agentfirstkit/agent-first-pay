#!/usr/bin/env bash
# Fixed test entrypoint shared by CI and the project gate.
set -euo pipefail

_AFDATA_BASH_SOURCE="$("${AFDATA_BIN:-afdata}" shell bash)"
# shellcheck source=/dev/stdin
source /dev/stdin <<<"$_AFDATA_BASH_SOURCE"
unset _AFDATA_BASH_SOURCE

ROOT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_PATH"

afdata_args_begin "$0 [MODE] [FILTER]"
afdata_args_positional test_mode MODE "static, unit, features, postgres, or all" optional
afdata_args_positional test_filter FILTER "Unit test filter" optional
afdata_args_parse "$@"
MODE="${test_mode:-all}"
FILTER="${test_filter:-}"
case "$MODE" in
  static|unit|features|postgres|all) ;;
  *)
    afdata_error invalid_test_mode "MODE must be static, unit, features, postgres, or all" || :
    exit 2
    ;;
esac
if [ -n "$FILTER" ] && [ "$MODE" != unit ] && [ "$MODE" != all ]; then
  afdata_error invalid_test_filter "FILTER is supported only by unit and all modes" || :
  exit 2
fi

TEST_TMP="$(mktemp -d "${TMPDIR:-/tmp}/agent-first-pay-test.XXXXXX")"
TEST_CONTAINER_NAME=""
TEST_CONTAINER_RUNTIME=""
cleanup() {
  local failed=0
  if [ -n "$TEST_CONTAINER_NAME" ]; then
    case "$TEST_CONTAINER_RUNTIME" in
      apple)
        if ! container delete --force "$TEST_CONTAINER_NAME" >/dev/null 2>&1; then
          afdata_log warn "Could not remove the temporary PostgreSQL container"
          failed=1
        fi
        ;;
      docker)
        if ! docker rm --force "$TEST_CONTAINER_NAME" >/dev/null 2>&1; then
          afdata_log warn "Could not remove the temporary PostgreSQL container"
          failed=1
        fi
        ;;
    esac
    TEST_CONTAINER_NAME=""
  fi
  if ! afdata_remove tmp_path "$TEST_TMP"; then
    failed=1
  fi
  return "$failed"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    # Native Rust processes read these environment paths without MSYS argv
    # conversion. Keep their state in the same directory cleanup removes.
    if ! TEST_TMP_WINDOWS="$(cygpath -m "$TEST_TMP")"; then
      afdata_error temporary_path_conversion_failed "Could not convert the temporary Windows path"
    fi
    TEST_TMP="$TEST_TMP_WINDOWS"
    ;;
esac
afdata_cli guard tmp_path "$TEST_TMP" >/dev/null
export AFPAY_TEST_HOME="$TEST_TMP/home" AFPAY_HOME="$TEST_TMP/home"
export AFUI_CONFIG_DIR="$TEST_TMP/afui-config"
mkdir -p "$AFPAY_TEST_HOME" "$AFUI_CONFIG_DIR"

TARGET_PATH="${CARGO_TARGET_DIR:-$ROOT_PATH/target}"
BINARY="$TARGET_PATH/debug/afpay"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) BINARY="${BINARY}.exe" ;;
esac

check_cli_discovery() {
  "$BINARY" --help --output json >"$TEST_TMP/help.jsonl"
  "$BINARY" api export --help --output json >"$TEST_TMP/api-help.jsonl"
  "$BINARY" --version --output json >"$TEST_TMP/version.jsonl"
  afdata_run --quiet afdata_cli validate "$TEST_TMP/help.jsonl" --strict
  afdata_run --quiet afdata_cli validate "$TEST_TMP/api-help.jsonl" --strict
  afdata_run --quiet afdata_cli validate "$TEST_TMP/version.jsonl" --strict
}

check_openapi_contract() {
  local generated="$TEST_TMP/openapi"
  afdata_run --quiet "$BINARY" api export --directory "$generated" --force
  if ! diff -r openapi "$generated" >/dev/null; then
    afdata_error openapi_contract_drift \
      "openapi/ is out of date; run afpay api export --directory openapi --force"
  fi
  afdata_run --quiet afdata_cli lint openapi/openapi.json
  afdata_run --quiet afdata_cli lint openapi/schemas/index.json
}

run_static() {
  afdata_run --quiet "$ROOT_PATH/scripts/install-shellcheck.sh" --check "$ROOT_PATH"
  afdata_run --quiet afdata_cli skill validate skills/agent-first-pay
  afdata_run --quiet cargo fmt --all --check
  afdata_run --quiet cargo clippy --features rest,ln-nwc,ln-lnbits --all-targets -- -D warnings
  afdata_run --quiet cargo build --features rest --bin afpay
  check_cli_discovery
  check_openapi_contract
}

run_unit() {
  # The Windows leg also builds the same release profile it distributes.
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) afdata_run --quiet cargo build --release --features rest --bin afpay ;;
  esac
  if [ -n "$FILTER" ]; then
    afdata_run --quiet cargo test --features rest,ln-nwc,ln-lnbits --lib --bins --tests "$FILTER"
  else
    afdata_run --quiet cargo test --features rest,ln-nwc,ln-lnbits --lib --bins --tests
  fi
}

run_features() {
  local feature_set
  # Each alternative is checked independently, with an explicit storage
  # backend. The final six entries are the README's minimal build commands.
  for feature_set in \
    redb,ln-nwc \
    redb,ln-lnbits \
    redb,btc-core \
    redb,btc-electrum \
    redb,federation \
    redb,cashu \
    redb,ln-phoenixd \
    redb,sol \
    redb,evm \
    redb,btc-esplora \
    postgres,federation; do
    afdata_log info "Checking isolated features: $feature_set"
    afdata_run --quiet cargo check --no-default-features --features "$feature_set"
  done
}

select_test_container_runtime() {
  TEST_CONTAINER_RUNTIME="${AFPAY_TEST_CONTAINER_RUNTIME:-}"
  if [ -z "$TEST_CONTAINER_RUNTIME" ]; then
    if [ "${CI:-}" != true ] && [ "$(uname -s)" = Darwin ]; then
      TEST_CONTAINER_RUNTIME=apple
    else
      TEST_CONTAINER_RUNTIME=docker
    fi
  fi
  case "$TEST_CONTAINER_RUNTIME" in
    apple)
      command -v container >/dev/null 2>&1 || {
        afdata_error missing_container_runtime "PostgreSQL tests require Apple container" || :
        return 2
      }
      afdata_run --quiet container system start
      ;;
    docker)
      command -v docker >/dev/null 2>&1 || {
        afdata_error missing_container_runtime "PostgreSQL tests require Docker" || :
        return 2
      }
      afdata_run --quiet docker info
      ;;
    *)
      afdata_error invalid_container_runtime "AFPAY_TEST_CONTAINER_RUNTIME must be apple or docker" || :
      return 2
      ;;
  esac
}

start_test_database() {
  select_test_container_runtime
  TEST_CONTAINER_NAME="afpay-test-pg-${PPID}-$$-${RANDOM}"
  local image="${AFPAY_TEST_POSTGRES_IMAGE:-postgres:16}"
  # These credentials belong only to this disposable database. Never reuse a
  # caller's DSN or publish a database on the usual host PostgreSQL port.
  export POSTGRES_PASSWORD=test
  case "$TEST_CONTAINER_RUNTIME" in
    apple)
      afdata_run --quiet container run --detach --rm --name "$TEST_CONTAINER_NAME" \
        --env POSTGRES_USER=test --env POSTGRES_PASSWORD --env POSTGRES_DB=test "$image"
      ;;
    docker)
      afdata_run --quiet docker run --detach --rm --name "$TEST_CONTAINER_NAME" \
        --env POSTGRES_USER=test --env POSTGRES_PASSWORD --env POSTGRES_DB=test \
        --publish 127.0.0.1::5432 "$image"
      ;;
  esac

  local ready=false attempt=0
  while [ "$attempt" -lt 120 ]; do
    if [ "$TEST_CONTAINER_RUNTIME" = apple ]; then
      if container exec "$TEST_CONTAINER_NAME" \
        pg_isready -h 127.0.0.1 -p 5432 -U test -d test >/dev/null 2>&1; then
        ready=true
        break
      fi
    elif docker exec "$TEST_CONTAINER_NAME" \
      pg_isready -h 127.0.0.1 -p 5432 -U test -d test >/dev/null 2>&1; then
      ready=true
      break
    fi
    attempt=$((attempt + 1))
    sleep 0.5
  done
  if [ "$ready" != true ]; then
    afdata_error postgres_not_ready "Temporary PostgreSQL did not become ready within 60 seconds"
  fi

  local address address_cidr port
  case "$TEST_CONTAINER_RUNTIME" in
    apple)
      address_cidr="$(container inspect "$TEST_CONTAINER_NAME" | afdata_cli value - 0.status.networks.0.ipv4Address)"
      address="${address_cidr%%/*}"
      if [[ ! "$address" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        afdata_error postgres_address_missing "Temporary PostgreSQL has no private IPv4 address"
      fi
      port=5432
      ;;
    docker)
      address=127.0.0.1
      port="$(docker inspect "$TEST_CONTAINER_NAME" | afdata_cli value - '0.NetworkSettings.Ports.5432/tcp.0.HostPort')"
      if [[ ! "$port" =~ ^[0-9]+$ ]] || [ "$port" = 5432 ]; then
        afdata_error postgres_port_invalid "Temporary PostgreSQL has no isolated host port"
      fi
      ;;
  esac
  export AFPAY_TEST_POSTGRES_DSN_SECRET="postgresql://test:test@${address}:${port}/test?sslmode=disable"
  afdata_log info "PostgreSQL regression uses a disposable $TEST_CONTAINER_RUNTIME container"
}

run_postgres() {
  start_test_database
  afdata_run --quiet cargo test --no-default-features --features postgres,ln-phoenixd \
    --lib postgres_backup_password_round_trip -- --ignored
  afdata_run --quiet cargo test --no-default-features --features postgres,ln-phoenixd \
    --test postgres_idempotency -- --ignored
}

case "$MODE" in
  static) run_static ;;
  unit) run_unit ;;
  features) run_features ;;
  postgres) run_postgres ;;
  all) run_static; run_unit; run_features; run_postgres ;;
esac
if ! cleanup; then
  trap - EXIT
  afdata_error test_cleanup_failed "Could not clean up all temporary test resources"
fi
trap - EXIT
afdata_result "Tests complete [$MODE]"
