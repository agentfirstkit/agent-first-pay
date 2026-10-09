# Testing

Use the fixed entrypoint from the repository root:

```bash
./scripts/test.sh all
```

CI and the kit project gate call this same script. It creates temporary wallet
and UI configuration directories and removes them when the run finishes.

## Modes

| Command | Coverage |
|---|---|
| `./scripts/test.sh static` | Pinned ShellCheck, skill validation, formatting, Clippy, CLI discovery, and committed OpenAPI/schema drift |
| `./scripts/test.sh unit` | Default-feature library, binary, and offline integration suites |
| `./scripts/test.sh unit FILTER` | The same suites restricted to a Rust test-name filter |
| `./scripts/test.sh features` | Eleven independent, no-default-feature storage/provider combinations, including the README's minimal builds |
| `./scripts/test.sh postgres` | PostgreSQL response-loss, idempotency-finalization failure, and cleanup-failure regressions |
| `./scripts/test.sh all` | All four modes in order |

Static checks require the `afdata` toolkit with skill support. PostgreSQL
tests use Apple `container` on local macOS and Docker on Linux/CI;
`AFPAY_TEST_CONTAINER_RUNTIME=apple` or `docker` selects the runtime
explicitly. The script provisions a fresh PostgreSQL 16 container, waits for
readiness, passes its test DSN through an environment variable, and cleans up
the container. It never connects to a caller's database.

## Payment safety regressions

Offline HTTP and JSON-RPC fixtures exercise Phoenixd success followed by failed
detail lookup, response loss after accepting a payment, Solana/EVM re-broadcast of the
same signed bytes to fallback endpoints without re-signing, and multi-node
receipt warning propagation.
Redb and PostgreSQL fixtures exercise failed idempotency writes, restart/replay,
Pending protection past the usual TTL, and explicit reservation reconciliation.
Plan-file tests check private permissions and atomic publication.

Unknown submissions preserve the Pending key and accounting reservation. A
successful payment whose idempotency write fails retains its successful receipt
with `idempotency_finalize_failed: true`; upstream nodes preserve that warning
on replay. These assertions use local fixtures, without sending real funds.

The default suite also covers offline BTC wallet/transaction regressions and
the REST API. Windows CI builds the release binary, runs the same offline
suite, and checks isolated features. The UI fixture that executes `/bin/sh`
is Unix-only.

Tests named `cashu_live`, `sol_live`, `evm_live`, and `btc_live`, including funded
send variants, remain ignored by this gate. Passing the offline gate does not
claim a live-wallet or live-chain test.
