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
- **Remote** (`RemoteProvider`) — serializes trait method calls to JSON, encrypts with AES-256-GCM, and sends via gRPC to a remote `afpay --mode rpc` daemon.

The coordinator's `config.toml` maps networks to named `afpay_rpc` nodes. Multiple networks can share the same node:

```toml
[afpay_rpc.wallet-server]
endpoint = "10.0.1.5:9400"
endpoint_secret = "abc..."

[afpay_rpc.chain-server]
endpoint = "10.0.1.6:9400"
endpoint_secret = "def..."

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
# Full stack — MCP server with all networks
afpay --mode mcp

# REST API server (curl-accessible, no specialized client needed)
afpay --mode rest --rest-api-key "my-secret"

# Or selective features
cargo build --features cashu
cargo build --features cashu,rest   # with REST API
cargo build --features btc-esplora
```

### Multi-Level (Cascading RPC)

Networks run as independent daemons. A coordinator connects to named `afpay_rpc` nodes via encrypted gRPC. Any node can itself forward to downstream nodes (cascading):

```
Agent (Claude)
  │ MCP (stdio)
  ▼
afpay --mode mcp                           ← coordinator (config.toml below)
  │ gRPC (AES-256-GCM PSK)
  ├──→ afpay --mode rpc (wallet-server)    ← VPS-A: ln + cashu
  └──→ afpay --mode rpc (chain-server)     ← VPS-B: sol + evm + btc
```

Coordinator `config.toml`:

```toml
[afpay_rpc.wallet-server]
endpoint = "vps-a:9400"
endpoint_secret = "abc..."

[afpay_rpc.chain-server]
endpoint = "vps-b:9400"
endpoint_secret = "def..."

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
- **Cascading limits** — each RPC layer enforces its own spend limits independently

### CLI Local vs Remote

The same CLI commands work locally or against a remote daemon:

```bash
# Local (wallet on this machine)
afpay send --network ln --to lnbc1...

# Remote (forward to rpc daemon)
afpay --rpc-endpoint 10.0.1.5:9400 --rpc-secret "abc..." send --network ln --to lnbc1...
```

With `--rpc-endpoint`, the CLI forwards the request. Without it, the CLI executes locally. Transparent to the caller.

## RPC Protocol

The RPC mode uses gRPC with PSK (Pre-Shared Key) payload encryption instead of TLS. A single secret handles both authentication and encryption with zero certificate management. Suitable for internal process-to-process communication where the operator controls all nodes.

### Proto Definition

```protobuf
syntax = "proto3";
package afpay;

service AfPay {
  rpc Call (EncryptedRequest) returns (EncryptedResponse);
}

message EncryptedRequest {
  bytes nonce = 1;       // 12 bytes, randomly generated per request
  bytes ciphertext = 2;  // AES-256-GCM(secret, JSON payload)
}

message EncryptedResponse {
  bytes nonce = 1;
  bytes ciphertext = 2;
}
```

The proto does not define business fields. The internal payload is just Input/Output JSON, encrypted before transport.

### Encryption Flow

```
Client                                Server
  │                                     │
  ├─ Input → serde_json::to_vec()       │
  ├─ AES-256-GCM encrypt(secret, nonce) │
  ├─ gRPC Call(nonce, ciphertext) ─────→│
  │                                     ├─ decrypt(secret, nonce, ciphertext)
  │                                     ├─ failure → disconnect (decrypt fail = auth fail)
  │                                     ├─ success → serde_json::from_slice() → handle
  │                                     ├─ Output → serialize → encrypt
  │ ←── gRPC Response(nonce, ct) ───────┤
  ├─ decrypt → Output                   │
```

### Configuration

```bash
# Daemon
afpay --mode rpc --rpc-listen 0.0.0.0:9400 --rpc-secret "64-char-hex"

# CLI direct to remote daemon
afpay --rpc-endpoint vps-a:9400 --rpc-secret "abc123..." send --wallet w_01 ...
```

For multi-level (coordinator → daemon), configure `config.toml` with named `afpay_rpc` nodes (see Deployment Patterns above). Each node can have a different secret. Secrets use the `_secret` suffix and are auto-redacted in agent-first-data output.

### Dependencies

```toml
tonic = "0.14"           # gRPC server/client
prost = "0.14"           # protobuf
tonic-build = "0.14"     # build.rs proto compilation
aes-gcm = "0.10"         # AES-256-GCM encryption
rmcp = "1.1"             # MCP framework (mcp feature)
schemars = "1"           # JSON Schema for MCP tool params (mcp feature)
axum = "0.8"             # HTTP REST server (rest feature)
tower-http = "0.6"       # CORS middleware (rest feature)
```

## REST API

The REST mode (`--mode rest`) provides a plain HTTP API with Bearer token authentication. Unlike the RPC mode (gRPC + AES-256-GCM), REST mode is designed for direct access from any HTTP client — no specialized client or encryption library needed.

### Protocol

```
POST /v1/afpay
Authorization: Bearer <api-key>
Content-Type: application/json

← Input JSON (same as pipe protocol, {"code":"...", ...})
→ Output[] JSON array
```

### Enforcement

Same as RPC mode:

| Rule | Behavior |
|------|----------|
| Spend limits | Always enforced |
| `is_local_only()` operations | Rejected with HTTP 403 |
| Authentication | Bearer token or X-API-Key header |

### Docker Deployment

The `docker/` directory provides a single-container deployment using supervisord. The `AFPAY_MODE` environment variable selects the afpay run mode (`rest`, `rpc`, or `mcp`):

```
supervisord
  ├─ [priority=10] bitcoind (optional)
  ├─ [priority=10] phoenixd (optional)
  ├─ [priority=20] afpay --mode $AFPAY_MODE
  └─ [priority=30] docker-setup.sh (one-shot, REST mode only: auto-creates wallets)
```

| Layer | Variable | Default | Description |
|-------|----------|---------|-------------|
| Build | `FEATURES` | `btc-core,ln-phoenixd,cashu,redb,mcp,rest,exchange-rate` | cargo --features |
| Build | `INSTALL_PHOENIXD` | `false` | Install phoenixd binary |
| Build | `INSTALL_BITCOIND` | `false` | Install bitcoind binary |
| Runtime | `AFPAY_MODE` | `rest` | afpay run mode: `rest`, `rpc`, `mcp` |
| Runtime | `AFPAY_PORT` | `9401` | Listen port (rest/rpc; ignored for mcp) |
| Runtime | `AFPAY_REST_API_KEY` | auto-generated | REST Bearer token (rest mode) |
| Runtime | `AFPAY_RPC_SECRET` | auto-generated | RPC PSK secret (rpc mode) |
| Runtime | `ENABLE_PHOENIXD` | `false` | Start phoenixd process |
| Runtime | `ENABLE_BITCOIND` | `false` | Start bitcoind process |

Secrets are auto-generated on first run and persisted to the data volume. The entrypoint prints connection info (endpoint + secret/key) to stdout on startup.

```bash
# REST mode (default) — curl-accessible
docker compose up --build

# RPC mode — for afpay CLI clients
AFPAY_MODE=rpc AFPAY_PORT=9400 docker compose up --build

# MCP mode — stdio for AI agents
AFPAY_MODE=mcp docker compose up --build
```

All commands work with Podman — replace `docker compose` with `podman compose`:

```bash
podman compose up --build
AFPAY_MODE=rpc AFPAY_PORT=9400 podman compose up --build

# Or build and run without compose
podman build -t afpay -f docker/Dockerfile .
podman run -d --name afpay -p 9401:9401 \
  -v afpay-data:/data/afpay -v bitcoind-data:/data/bitcoind -v phoenixd-data:/data/phoenixd \
  -e AFPAY_MODE=rest afpay

# Management
podman exec -it afpay supervisorctl status
podman logs afpay
```

## Spend Limits

Multi-tier sliding window limits. All rules are checked before every send — any breach rejects the transaction with `LimitExceeded`.

### Enforcement Model

Each node decides independently whether to enforce limits:

| Mode | Enforcement | Rationale |
|------|------------|-----------|
| `--mode rpc` | Always enforced | Security boundary — agent cannot modify daemon config |
| `--mode rest` | Always enforced | Security boundary — same as RPC mode |
| CLI/pipe/MCP + all local providers | Enforced | Only defense layer available |
| CLI/pipe/MCP + any remote provider | Not enforced locally | Remote daemon handles it |

In cascading deployments, each RPC daemon layer enforces its own limits. The coordinator delegates enforcement to downstream nodes.

### Downstream Limit Querying

`limit list` queries this node's limits AND each downstream `afpay_rpc` node's limits recursively, assembling a tree:

```json
{
  "code": "limit_status",
  "limits": [ ... ],
  "downstream": [
    {
      "name": "wallet-server",
      "endpoint": "10.0.1.5:9400",
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
cargo build --no-default-features --features postgres,mcp,exchange-rate

# Pure coordinator (only RPC forwarding, no wallet SDK, no local storage)
cargo build --no-default-features --features mcp
```

### SDK Dependencies

| Component | Crate | Notes |
|-----------|-------|-------|
| Cashu | `cdk` (Cashu Dev Kit) | Pure Rust, HTTP mint interaction |
| Lightning | phoenixd / LNbits / NWC | External backends, no embedded node |
| Solana | anza-xyz component crates v3.x | Pure Rust (not monolithic solana-sdk) |
| EVM | `alloy` | Pure Rust (no kzg feature) |
| Bitcoin (Esplora) | `bdk_wallet` + `bdk_esplora` | BDK v2, Esplora HTTP API, SegWit/Taproot |
| Bitcoin (Core RPC) | `bdk_wallet` + `bdk_bitcoind_rpc` | BDK v2, bitcoind JSON-RPC |
| Bitcoin (Electrum) | `bdk_wallet` + `bdk_electrum` | BDK v2, Electrum protocol |
| Storage (embedded) | `redb` | Embedded key-value, pure Rust |
| Storage (PostgreSQL) | `sqlx` | Async PostgreSQL, pure Rust (rustls) |
| MCP | `rmcp` | MCP server framework (stdio transport) |
