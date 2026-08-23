# Architecture

## Provider Trait

All network backends implement the same trait:

```rust
#[async_trait]
pub trait PayProvider: Send + Sync {
    fn network(&self) -> Network;

    async fn create_wallet(&self, req: WalletCreateRequest) -> Result<WalletInfo, PayError>;
    async fn create_ln_wallet(&self, req: LnWalletCreateRequest) -> Result<WalletInfo, PayError>;
    async fn close_wallet(&self, wallet: &str) -> Result<(), PayError>;
    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError>;
    async fn balance(&self, wallet: &str) -> Result<BalanceInfo, PayError>;
    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError>;
    async fn receive_info(&self, wallet: &str, amount: Option<&Amount>, memo: Option<&str>) -> Result<ReceiveInfo, PayError>;
    async fn receive_claim(&self, wallet: &str, quote_id: &str) -> Result<u64, PayError>;
    async fn cashu_send(&self, wallet: Option<&str>, amount: &Amount, ...) -> Result<CashuSendResult, PayError>;
    async fn cashu_receive(&self, wallet: &str, token: &str) -> Result<CashuReceiveResult, PayError>;
    async fn send(&self, wallet: &str, to: &str, amount: &Amount, ...) -> Result<SendResult, PayError>;
    async fn history_list(&self, wallet: Option<&str>, ...) -> Result<Vec<HistoryRecord>, PayError>;
    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError>;
    async fn history_sync(&self, wallet: &str, limit: usize) -> Result<HistorySyncStats, PayError>;
    // ... restore, check_balance, send_quote, etc.
}
```

Two backend types implement this trait:

- **Local** — compiled with the corresponding feature flag (e.g. `cashu`, `ln`). Wallet SDK runs in-process.
- **Remote** (`RemoteProvider`) — an HTTP client against another afpay node's `/v1` resource routes, carrying that node's Bearer API key. There is no afpay-specific transport: federation and `curl` use the same face, so a peer can only reach what any agent holding that token could reach.

The coordinator's `config.toml` maps networks to named peers. Multiple networks can share the same peer:

```toml
[peers.wallet-server]
url = "http://10.0.1.5:9401"
api_key_secret = "abc..."

[peers.chain-server]
url = "http://10.0.1.6:9401"
api_key_secret = "def..."

[providers]
cashu = "wallet-server"
ln = "wallet-server"
sol = "chain-server"
evm = "chain-server"
btc = "chain-server"   # any btc backend (esplora/core-rpc/electrum)
```

Networks not listed in `[providers]` use their local implementation (if compiled in). This makes local and remote execution transparent to callers.

## Deployment Patterns

### Single Machine

All networks in one process. Simplest setup:

```bash
# HTTP API server (curl-accessible, no specialized client needed)
afpay --mode rest --rest-api-key-secret "my-secret"       # 127.0.0.1:9401 by default

# Or selective features
cargo build --features cashu
cargo build --features cashu,rest   # with the HTTP API
cargo build --features btc-esplora
```

### Multi-Level (Cascading Federation)

Networks run as independent daemons. A coordinator forwards to named peers over
their HTTP APIs. Any node can itself forward to downstream peers (cascading):

```
Agent / Client
  │ HTTP + Bearer
  ▼
afpay --mode rest                          ← coordinator (config.toml below)
  │ HTTP + Bearer (the peers' own /v1 routes)
  ├──→ afpay --mode rest (wallet-server)   ← VPS-A: ln + cashu
  └──→ afpay --mode rest (chain-server)    ← VPS-B: sol + evm + btc
```

Every hop is the same protocol, so a coordinator is just a client with a config
file. Each leg needs its own encrypted path — see
[Reaching a daemon that is not on this machine](../README.md#reaching-a-daemon-that-is-not-on-this-machine).

Coordinator `config.toml`:

```toml
[peers.wallet-server]
url = "http://vps-a:9401"
api_key_secret = "abc..."

[peers.chain-server]
url = "http://vps-b:9401"
api_key_secret = "def..."

[providers]
ln = "wallet-server"
cashu = "wallet-server"
sol = "chain-server"
evm = "chain-server"
btc = "chain-server"   # any btc backend (esplora/core-rpc/electrum)
```

Benefits:
- **Fault isolation** — one network crashing doesn't affect others
- **Minimal attack surface** — each container only has the SDK for its network
- **Independent scaling** — hot wallets on fast VPS, cold storage on secure hardware
- **Cascading limits** — each layer enforces its own spend limits independently

### CLI Local vs Federated

The same CLI commands work locally or against a peer:

```bash
# Local (wallet on this machine). `send` resolves a plan and pays nothing.
afpay ln send --to lnbc1...
afpay pay confirm --plan-id plan_… --idempotency-key pay-invoice-1

# On a peer (forwarded over its HTTP API) — both halves, same as local
afpay ln send --to lnbc1... --peer-url http://10.0.1.5:9401 --peer-api-key-secret "abc..."
afpay pay confirm --plan-id plan_… --peer-url http://10.0.1.5:9401 --peer-api-key-secret "abc..."
```

With `--peer-url`, the CLI forwards the request. Without it, the CLI executes locally. Transparent to the caller.

### Paying is two commands

`afpay <network> send` resolves the payment — the wallet it would use, what
leaves it, the fee, the spend budgets it would debit, and any structured
`warnings` — and prints a `plan_id`. Warnings are part of `pay_planned`, not a
filterable log. It contacts nothing that could move value. `afpay pay confirm
--plan-id …` is what pays, and it is the only command that does.

The split is not CLI politeness. `Input::Send` no longer exists: the dispatcher
every mode shares has exactly one operation that moves money out of a wallet,
and it takes a plan id. A plan is single-use, expires after 15 minutes, and is
refused (`plan_stale`) if the workspace, daemon configuration, wallet metadata
or spend-limit rules changed after it was resolved — so a payment that was
reviewed and a payment that happens cannot be two different payments.

An explicit EVM `chain_id` mismatch is a hard refusal. Omitting it produces an
`evm_chain_unpinned` plan warning. Solana cluster metadata is weaker: endpoint
hostnames can offer evidence, but private or proxied RPC names cannot prove a
cluster. Unpinned, unclassifiable, and apparently mismatched plans therefore
carry review warnings instead of pretending that a hostname is an on-chain
attestation.

If the network accepted a payment but the spend ledger could not confirm its
reservation, the request has exactly one terminal outcome:
`accounting_inconsistent`. It includes the transaction and reservation IDs,
must not be retried, and is never followed by `sent` or `cashu_sent`. Reconcile
the named reservations locally before making another payment.

## Federation

`<command> --peer-url` runs a command on another afpay node. The wire is that
node's own HTTP domain API — the routes documented below — authenticated with
its `--rest-api-key-secret`. There is no handshake, no session table, and no
payload cipher: one command is one HTTP request.

```bash
# Peer (the same daemon any HTTP client would talk to)
afpay --mode rest --rest-api-key-secret "64-char-hex"

# A command run on that peer
afpay ln send --wallet w_01 ... \
  --peer-url http://vps-a:9401 --peer-api-key-secret "64-char-hex"
```

For a coordinator, put the peers in `config.toml` instead (see Deployment
Patterns above). Each peer can have a different key; keys use the `_secret`
suffix and are auto-redacted in agent-first-data output.

### What federation can and cannot ask for

A peer is reached through the published routes and nothing else. Every operation
`Input::is_local_only` marks — seed material, spend-limit rule writes,
reservation reconcile, wallet-config writes, `wallet restore` — is refused by the
client *before any bytes are sent*, and has no route on the peer either. That
symmetry is the point: a leaked bearer cannot raise its own spending limit,
whether it belongs to an agent or to another afpay node.

`limit list` is the one policy read that crosses the hop, and it crosses as
`GET /v1/spend-limits` — a read any token holder already has.

### Mismatched peers fail loudly

There is no cross-version compatibility layer. A peer that is not this afpay is
named, not guessed at:

| `error_code` | Meaning |
|--------------|---------|
| `peer_unreachable` | Nothing answered. Retryable; names the URL. |
| `peer_not_afpay` | Something answered, but not with afpay's protocol envelope. Reports the HTTP status, content type, and a snippet of the body. |
| `peer_route_unsupported` | An afpay that does not serve this route — i.e. a different version. Names the method and path. |
| `peer_unauthorized` | The credential was refused; names `--peer-api-key-secret`. |
| `peer_mismatch` | `GET /health` reported a different afpay version or protocol version. Long-lived modes run this check at startup for every configured peer and refuse to serve. |

### Transport security

Federation carries no encryption of its own. Run each leg over Tailscale or
WireGuard, an SSH tunnel, or a TLS reverse proxy — the same three arrangements
any HTTP client uses, documented with copy-pasteable configuration in
[Reaching a daemon that is not on this machine](../README.md#reaching-a-daemon-that-is-not-on-this-machine).
Encryption is not authentication: the bearer token is required in every case.

### Dependencies

```toml
axum = "0.8"             # HTTP server (rest feature)
schemars = "1.2"         # OpenAPI / JSON Schema generation (rest feature)
reqwest = "0.13"         # HTTP client (federation feature)
```

### Public Listen Policy

`--rest-listen` defaults to `127.0.0.1`. Binding to `0.0.0.0`, `::`, or another non-loopback address fails unless `--public-listen` is also supplied. Treat `--public-listen` as an operational acknowledgement, not a security control: afpay serves plain HTTP and terminates no TLS of its own. See the README section linked above for the three sanctioned ways to carry it.

## HTTP API

`--mode rest` serves afpay's domain as HTTP resources, described by an OpenAPI 3.2 document the daemon serves and the repository commits. It is the only machine face afpay has: agents, containers, and other afpay nodes all speak it, and it needs no specialized client. It listens on loopback by default; reach it from elsewhere through Tailscale/WireGuard, an SSH tunnel, or a TLS reverse proxy — see [Reaching a daemon that is not on this machine](../README.md#reaching-a-daemon-that-is-not-on-this-machine).

### Discovery face

Public, credential-free, and answerable offline — an agent can read the whole contract before it holds a token:

| Method | Path | Meaning |
|--------|------|---------|
| `GET` | `/health` | Service name, version, protocol version, readiness |
| `GET` | `/openapi.json` | The OpenAPI document this process actually serves |
| `GET` | `/schemas/index.json` | Index of the standalone JSON Schemas |
| `GET` | `/schemas/{schema_file}` | One `application/schema+json` document |

`afpay api export --directory openapi --force` writes the same three artifacts without starting anything; they are committed under `openapi/` and a drift test fails if they disagree with the Rust DTOs they came from.

### Resource routes

Every route below requires `Authorization: Bearer <api-key>` and reaches the same dispatcher, spend ledger, and store the CLI reaches.

| Method | Path | Operation |
|--------|------|-----------|
| `GET` | `/v1/wallets` | List wallets (`?network=`) |
| `POST` | `/v1/wallets` | Create a wallet — closed tagged union on `network`, **`Idempotency-Key` required** |
| `GET` | `/v1/wallets/{wallet}` | Read one wallet's stored configuration |
| `DELETE` | `/v1/wallets/{wallet}` | Close a wallet |
| `GET` | `/v1/balances` | Balances across wallets (`?wallet=`, `?network=`, `?check=`) |
| `POST` | `/v1/receives` | Create an address, invoice, or mint quote — **`Idempotency-Key` required** |
| `POST` | `/v1/receives/{quote_id}/claim` | Claim a paid mint quote |
| `POST` | `/v1/send-plans` | Resolve a payment into a reviewable plan — nothing moves |
| `POST` | `/v1/sends` | Pay by confirming a plan — body is `{"plan_id"}`, **`Idempotency-Key` required** |
| `POST` | `/v1/cashu/token-plans` | Resolve a token mint into a reviewable plan — nothing moves |
| `POST` | `/v1/cashu/tokens` | Mint by confirming a plan — body is `{"plan_id"}`, **`Idempotency-Key` required** |
| `POST` | `/v1/cashu/redemptions` | Redeem a Cashu bearer token |
| `GET` | `/v1/transactions` | List recorded payments |
| `GET` | `/v1/transactions/{transaction_id}` | One payment's settlement status |
| `POST` | `/v1/transactions/sync` | Re-read provider activity into local history |
| `GET` | `/v1/spend-limits` | Read every rule and the spend consumed in its window |

### Envelopes

Every domain response is a strict AFDATA envelope, redacted at the serialization boundary, with the request correlation id in the `x-request-id` header:

```
{"kind":"result","result":{…},"trace":{"duration_ms":3}}
{"kind":"error","error":{"code":"wallet_not_found","message":"…","retryable":false,"hint":"…"},"trace":{"duration_ms":1}}
```

The HTTP status and `error.code` are both load-bearing: `400` input, `401` credential, `403` operator policy, `404` resource, `405` method, `409` idempotency conflict, `413` body, `415` media type, `422` valid but inapplicable (a spend-limit refusal lands here), `429` rate limit, `500` internal or ledger failure, `503` provider unreachable.

Two afpay outputs that the wire protocol carries as results become errors here, because reporting them as successes would misstate the business outcome: `limit_exceeded` (a spend rule refused the payment) and `accounting_inconsistent` (money left but the ledger could not record it). Both carry their payload as `error.details`.

### Enforcement

| Rule | Behavior |
|------|----------|
| Spend limits | Always enforced, through the same reserve/execute/confirm path as the CLI |
| Plan/confirm | Money leaves a wallet only by confirming a plan afpay resolved and recorded. The confirm body carries the id and nothing else, so an approved payment and the payment made cannot differ |
| Idempotency | `Idempotency-Key` becomes the ledger key `--idempotency-key` writes: same 24-hour window, same canonical body hash, same replay. Required on the four operations a retry could duplicate — both confirms, wallet creation, and receives |
| `is_local_only()` operations | Not routed at all — seeds, spend-limit rules, reservation repair and daemon config exist only on the local CLI |
| Authentication | `Authorization: Bearer` only; credentials in the query string are refused |
| CORS | No header emitted; a browser origin is authorized by same-host proxying, not by afpay |

### Container Deployment

The `container/docker/` directory provides the canonical single-container deployment using supervisord (one merged `Dockerfile` whose `AFPAY_BIN_FROM` build-arg selects a `downloader` or `builder` source stage). The `afpay container` command builds and runs it under Docker, Podman, or Apple `container`:

```
supervisord
  ├─ [priority=10] bitcoind (optional)
  ├─ [priority=10] phoenixd (optional)
  ├─ [priority=20] afpay --mode rest
  └─ [priority=30] container-setup.sh (one-shot: auto-creates wallets)
```

| Layer | Variable | Default | Description |
|-------|----------|---------|-------------|
| Build | `FEATURES` | `btc-core,ln-phoenixd,cashu,redb,rest,exchange-rate` | cargo --features |
| Build | `INSTALL_PHOENIXD` | `true` | Install phoenixd binary |
| Build | `INSTALL_BITCOIND` | `false` | Install bitcoind binary |
| Runtime | `AFPAY_PORT` | `9401` | Listen port |
| Runtime | `AFPAY_REST_API_KEY_SECRET` | auto-generated | HTTP API Bearer token; 32–512 bearer-safe ASCII characters |
| Runtime | `ENABLE_PHOENIXD` | `true` | Start phoenixd process |
| Runtime | `ENABLE_BITCOIND` | `false` | Start bitcoind process |
| Runtime | `BTC_NETWORK` | `mainnet` | bitcoind network |
| Runtime | `BTC_RPC_PORT` | `8332` | bitcoind RPC port |
| Runtime | `BTC_PRUNE_MB` | `550` | bitcoind prune target in MiB (`0` disables pruning) |

Secrets are auto-generated on first run and persisted to private files in the data volume. The entrypoint prints endpoint and secret file locations, but not secret values, and passes secrets through environment variables instead of process arguments.

```bash
docker compose -f container/docker/compose.yaml up --build
```

All commands work with Podman — replace `docker compose` with `podman compose`:

```bash
podman compose -f container/docker/compose.yaml up --build

# macOS Apple Container CLI launcher
./container/apple-container/up.sh

# Or build and run without compose
podman build -t afpay -f container/docker/Dockerfile .
podman run -d --name afpay -p 9401:9401 \
  -v afpay-data:/data/afpay -v bitcoind-data:/data/bitcoind -v phoenixd-data:/data/phoenixd \
  afpay

# Management
podman exec -it afpay supervisorctl status
podman logs afpay
```

## Panels (`afpay ui`)

`afpay ui …` opens a window on the person's machine and does not return until
they are done with it. Each panel runs the same request as the command it
mirrors, through the same handler, store, providers, spend ledger and
idempotency, and renders afpay's own emitted event — already redacted by the
`_secret` convention — rather than reaching back into the typed structs.

| `ui_kind` | Panel | Shape | Deliveries |
| --- | --- | --- | --- |
| `wallet_inspect` | `afpay ui wallet` | Watch: read it, close it | window, link, session |
| `receive_inspect` | `afpay ui receive` | Watch: point a phone at it | window, link, session |
| `send_confirm` | `afpay ui send` | Decide: one typed answer, and only one of them sends | window, session |

### Why the decision panel has one delivery fewer

AFUI's `link` is a URL that is itself the credential: whoever holds it reaches
the page. For a watch panel that is a view of balances or a receive code, bounded
by AFUI's own attention policy — a fair trade for being able to point a phone at
it, and the reason a receive panel exists at all.

For `afpay ui send` it would be the authority to move money, held by whoever
has the URL. So that panel does not offer it, and the refusal is in the argument
parser rather than at run time: a person who asks for a delivery this panel does
not do is told before a payment has been planned, not after. Answering a send
from another device is still available — deliver it as `session` and open it
through `afui session serve`, which is AFUI's front door with its own credential
rather than a link that is one.

### Replacing a panel (`afui frontend`)

Every panel is a MiniJinja template rendered against a typed document, and any
of them can be replaced without touching afpay. AFUI owns where an override
lives and whether it is trusted; afpay owns what the files mean. Install one
with `afui frontend init --provider-id afpay --ui-kind <KIND>` and turn it on
with `afui frontend enable`. **`ui_api_version` is `1`**, and it covers all
three panels: the documents below, the template names, the
`<!-- afpay:trusted-runtime -->` marker and the `data-afpay-decision`
declaration are one contract.

Files an override may supply, each independently — a file it does not supply
comes from afpay, so replacing one page keeps the rest:

| Path | What it is |
| --- | --- |
| `templates/page.html.j2` | The panel body for this `ui_kind` |
| `templates/layout.html.j2` | The frame every page extends |
| `templates/fields.html.j2` | The name/value row partial |
| `templates/decided.html.j2` | What `send_confirm` shows after an answer |
| `templates/<anything>.j2` | Partials of your own, reachable with `{% include %}` |

A template is registered under its full path, so `{% extends %}` and
`{% include %}` name it that way too: `{% extends "templates/layout.html.j2" %}`,
`{% include "templates/fields.html.j2" %}`. The same path a person edits is the
path a template refers to.
| `style.css` | The stylesheet |
| `assets/**` | Stylesheets, images and fonts, served from the session origin |

Templates render against `document`, which carries everything the panel worked
out — already counted, grouped, ordered and written as text. Every value is a
string, a boolean or a count; amounts are exact digits with no thousands
separators, and a value afpay has no answer for is an em dash rather than an
empty cell. Reorder it, regroup it, drop what you do not want; you cannot
arrive at a different answer than the one `afpay balance` or `afpay receive`
reports, because the panel and the command run the same request.

Every document carries `ui_kind`, `title`, `heading`, `subject` and `footer`,
plus:

| `ui_kind` | Document |
| --- | --- |
| `wallet_inspect` | `wallet_count`, `unreachable_count`, `totals[]` (`network`, `unit`, `confirmed`, `pending`, `wallet_count`, `errors`, `degraded`), `groups[]` (`network`, `wallets[]`) |
| | each wallet: `id`, `label`, `address`, `failed`, `error`, `balance` (`unit`, `confirmed`, `pending`, `extras[]`), `details[]` |
| `receive_inspect` | `network`, `wallet`, `scannable`, `qr` (`kind`, `url`, `alt`), `warning`, `payload[]`, `details[]` |
| `send_confirm` | `plan_id`, `operation`, `network`, `wallet`, `to`, `amount`, `fee`, `unit`, `debits[]` (`amount`, `token`), `details[]`, `decisions[]` (`id`, `label`) |
| after an answer | `message` |

`extras[]`, `details[]` and `payload[]` are lists of `{name, value}` — anything
a provider returned that the card did not lay out by hand, so a field a backend
starts reporting shows up rather than vanishing. `qr.url` is a route on the
session: afpay draws the code and serves it, a template decides where it sits
and how large it is.

Three things an override cannot do, and they are enforced rather than
requested:

- **Ship JavaScript.** AFUI refuses a frontend file whose name says it is a
  script, and refuses a `<script>`, an `onclick=`, or a `javascript:` URL
  inside a template. The only script any panel loads is afpay's own.
- **Decide what a control means.** `send_confirm` templates *declare* a control
  with `data-afpay-decision="approve"` or `"refuse"`; afpay's runtime, spliced
  in at the layout's `<!-- afpay:trusted-runtime -->` marker under a
  per-session nonce, is what binds that declaration to the route that sends or
  refuses. A declaration afpay does not recognise binds to nothing, and the
  match is exact — `approve-all` is not `approve`. A page that does not declare
  both controls, or leaves no room for the runtime, does not open. What gets
  paid is the plan that was shown, submitted by `plan_id` from afpay's own
  memory: a page cannot make an approval name a different payment.
- **Turn escaping off.** `|safe`, a `filter safe` block and `autoescape` are
  refused, and every value a panel prints — including anything a provider
  returned — is escaped.

A frontend afpay cannot load is an error naming safe mode
(`ui_frontend_incompatible`, `ui_frontend_unreadable`, `ui_frontend_unsafe`,
`ui_frontend_template`, `ui_frontend_incomplete`), never a quietly substituted
built-in page: no window opens, no payment is resolved on the confirm panel,
and nothing is sent. `AFUI_SAFE_MODE=1` ignores every override. A workspace
frontend that has not been enabled is skipped in silence by design — the
`ui_ready` progress event carries `ui_frontend_id` only when an override is
actually serving, which is how an agent tells "my override is running" from
"my override is inert" without opening a window to look.

## Spend Limits

Multi-tier sliding window limits. All rules are checked before every send — any breach rejects the transaction with `LimitExceeded`.

### Enforcement Model

Each node decides independently whether to enforce limits:

| Mode | Enforcement | Rationale |
|------|------------|-----------|
| `--mode rest` | Always enforced | Security boundary — agent cannot modify daemon config |
| CLI/pipe + all local providers | Enforced | Only defense layer available |
| CLI/pipe + any peer provider | Not enforced locally | The peer handles it |

In cascading deployments, every layer enforces its own limits. The coordinator delegates enforcement to downstream peers.

### Downstream Limit Querying

`limit list` queries this node's limits AND each downstream peer's limits recursively, assembling a tree:

```json
{
  "code": "limit_status",
  "limits": [ ... ],
  "downstream": [
    {
      "name": "wallet-server",
      "endpoint": "http://10.0.1.5:9401",
      "limits": [ ... ],
      "downstream": []
    }
  ]
}
```

`limit add`/`limit remove` only affect the local node. Each daemon manages its own limits independently.

### Tracking

Spend tracking uses a reservation-based model. Each send is first reserved against all matching limits (checking the sliding window), then confirmed or cancelled after the transaction completes.

**redb backend**: Rules, reservations, and events stored in local `spend.redb`. Single-process concurrency via in-process mutex.

**PostgreSQL backend**: Same data model stored in `spend_rules`, `spend_reservations`, `spend_events` tables. Multi-process concurrency via `pg_advisory_xact_lock` — the reserve operation acquires an advisory lock within a transaction to prevent concurrent check-then-write races.

Exchange rate quotes (for `global-usd-cents` scope) are cached in the storage backend — `exchange-rate-cache.redb` or the `exchange_rate_cache` PostgreSQL table.

Exchange-rate API credentials should use `api_key_secret` in `config.toml`; legacy `api_key` still deserializes for compatibility but new serialized configs use the `_secret` suffix for redaction.

### Scope Levels

| Scope | Granularity | Example |
|-------|-------------|---------|
| `wallet` | Per-wallet | `wallet:w_1a2b3c4d:1h:10000sats` |
| `network` | Per-network across all wallets | `network:cashu:1h:10000sats` |
| `all` | All networks (requires exchange rate) | `all:24h:5000usd` |

Supported units: `sats` (cashu/ln/btc), `lamports` (sol), `gwei`/`wei` (evm), `usd`. Native units for a network do not require exchange rate config; non-native units and `all`-scope rules always do.

## Compilation

Feature flags control which network SDKs and storage backends are compiled in:

```bash
# Single-network VPS daemon (minimal binary size)
cargo build --no-default-features --features ln,redb

# Full stack (all networks + all storage)
cargo build

# PostgreSQL-only server (no local redb)
cargo build --no-default-features --features postgres,exchange-rate

# Pure coordinator (only federation forwarding, no wallet SDK, no local storage)
cargo build --no-default-features --features federation
```

### SDK Dependencies

| Component | Crate | Notes |
|-----------|-------|-------|
| Cashu | `cdk` (Cashu Dev Kit) | Pure Rust, HTTP mint interaction |
| Lightning | phoenixd / LNbits / NWC | External backends, no embedded node. phoenixd supports BOLT12 offers |
| Solana | anza-xyz component crates v3.x | Pure Rust (not monolithic solana-sdk) |
| EVM | `alloy` | Pure Rust (no kzg feature) |
| Bitcoin (Esplora) | `bdk_wallet` + `bdk_esplora` | BDK v2, Esplora HTTP API, SegWit/Taproot |
| Bitcoin (Core RPC) | `bdk_wallet` + `bdk_bitcoind_rpc` | BDK v2, bitcoind JSON-RPC |
| Bitcoin (Electrum) | `bdk_wallet` + `bdk_electrum` | BDK v2, Electrum protocol |
| Storage (embedded) | `redb` | Embedded key-value, pure Rust |
| Storage (PostgreSQL) | `sqlx` | Async PostgreSQL, pure Rust (rustls) |
