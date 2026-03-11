# afpay

Cryptocurrency micropayment tool designed for AI agents. Cashu, Lightning, Solana, EVM, Bitcoin on-chain — single binary, unified CLI, with multi-tier spend limits (per-wallet, per-network, global).

Pure Rust, zero C dependencies.

## Architecture

All network wallets managed in one process via the Provider trait:

```
afpay <command>
  ├── cashu provider
  ├── ln provider
  ├── sol provider
  ├── evm provider
  └── btc provider
```

For remote operation, two server modes are available:

```
# RPC mode — gRPC + AES-256-GCM PSK (process-to-process)
afpay CLI / MCP / pipe
  │ gRPC (AES-256-GCM PSK)
  └──→ afpay --mode rpc (VPS)

# REST mode — HTTP + Bearer token (any HTTP client)
curl / scripts / any language
  │ HTTP POST /v1/afpay
  └──→ afpay --mode rest (VPS / Docker)
```

Both server modes enforce spend limits independently.

See [Architecture](docs/architecture.md) for advanced multi-server deployment patterns.

## Supported Networks

| Network | Unit | Token Support | Feature |
|---------|------|---------------|---------|
| Cashu | sats | — | `cashu` |
| Lightning | sats | — | `ln-nwc` / `ln-phoenixd` / `ln-lnbits` |
| Solana | lamports | USDC, USDT (SPL) | `sol` |
| EVM chain | gwei | USDC, USDT (ERC-20) | `evm` |
| Bitcoin | sats | — | `btc-esplora` / `btc-core` / `btc-electrum` |

All features enabled by default. Selective compilation:

```bash
cargo build --features cashu              # Cashu only (minimal binary)
cargo build --features cashu,ln           # Cashu + Lightning
cargo build --features btc-esplora        # Bitcoin on-chain (Esplora backend)
cargo build --features btc-core           # Bitcoin on-chain (Bitcoin Core RPC)
cargo build --features btc-electrum       # Bitcoin on-chain (Electrum)
cargo build --no-default-features         # RPC client only (no wallet SDK)
cargo build                               # All networks + all storage backends
```

## Storage Backends

| Backend | Feature | Use Case |
|---------|---------|----------|
| redb | `redb` (default) | Embedded key-value, single-process, zero config |
| PostgreSQL | `postgres` (default) | Multi-process, concurrent access, server deployments |

Both compiled by default. Select via `config.toml`:

```toml
storage_backend = "redb"       # default — embedded (no setup needed)

storage_backend = "postgres"   # PostgreSQL
postgres_url_secret = "postgres://user:pass@localhost/afpay"
```

PostgreSQL uses `pg_advisory_xact_lock` for spend limit concurrency safety. When `storage_backend = "postgres"`, all data — wallet metadata (including seed secrets), CDK proofs, transaction history, and spend accounting — is stored in PostgreSQL.

## Install

```bash
brew install cmnspore/tap/afpay   # macOS/Linux
scoop bucket add cmnspore https://github.com/cmnspore/scoop-bucket && scoop install afpay  # Windows
cargo install agent-first-pay     # any platform
```

Binary: `afpay`

## Usage

Default mode is CLI — one command per invocation:

```bash
# Local (wallet on this machine)
afpay balance

# Remote (forward to rpc daemon)
afpay --rpc-endpoint 10.0.1.5:9400 --rpc-secret "64-char-hex" balance
```

Other modes: `--mode interactive` (REPL), `--mode pipe` (JSONL stdin/stdout), `--mode mcp` (MCP stdio, rmcp framework), `--mode rpc` (gRPC daemon), `--mode rest` (HTTP REST API). See [Manual](docs/manual.md) for details.

## Quick Start

### Cashu

```bash
# Setup
afpay wallet create --network cashu --cashu-mint https://mint.minibits.cash/Bitcoin --label cashu-main

# Deposit (Lightning → Cashu)
afpay receive --network cashu --amount 1000
# → returns invoice + quote_id; pay the invoice, then claim:
afpay receive --network cashu --ln-quote-id <quote_id>

# Send P2P cashu token
afpay send --wallet cashu-main --amount 21 --local-memo "coffee"
# → returns cashu token string
# Or filter by mint URL (picks first wallet with sufficient balance):
afpay send --network cashu --cashu-mint https://mint.minibits.cash/Bitcoin --amount 21

# Receive cashu token (auto-matches wallet by mint URL in token)
afpay receive --cashu-token "cashuBo2F..."

# Send to Lightning invoice
afpay send --network cashu --to lnbc1...

afpay balance --network cashu
```

### Lightning

```bash
# Setup (choose one backend)
afpay wallet create --network ln --backend nwc --nwc-uri-secret "nostr+walletconnect://..."
afpay wallet create --network ln --backend phoenixd --endpoint http://localhost:9740 --password-secret "hunter2"
afpay wallet create --network ln --backend lnbits --endpoint https://legend.lnbits.com --admin-key-secret "abc123"

# Receive (create invoice)
afpay receive --network ln --amount 500

# Send (pay invoice)
afpay send --network ln --to lnbc1...

afpay balance --network ln
```

### Solana

```bash
# Setup
afpay wallet create --network sol --sol-rpc-endpoint https://api.mainnet-beta.solana.com --label sol-main

# Native SOL
afpay send --network sol --to <address> --amount 1000000 --token native
afpay receive --wallet sol-main --wait --amount 1000000 --token native

# SPL token (USDC — built-in)
afpay send --network sol --to <address> --amount 1000000 --token usdc
afpay receive --wallet sol-main --wait --amount 1000000 --token usdc

# Custom SPL token (register first, then use by symbol)
afpay wallet config token-add --wallet sol-main --symbol bonk --address DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263 --decimals 5
afpay send --network sol --to <address> --amount 100000 --token bonk
afpay receive --wallet sol-main --wait --amount 100000 --token bonk

afpay balance --network sol
```

### EVM Chain

```bash
# Setup
afpay wallet create --network evm --evm-rpc-endpoint https://mainnet.base.org --label evm-base
afpay wallet create --network evm --evm-rpc-endpoint https://arb1.arbitrum.io/rpc --evm-chain-id 42161 --label evm-arb

# Native ETH
afpay send --wallet evm-base --to <address> --amount 1000000000000 --token native
afpay receive --wallet evm-base --wait --amount 1000000000000 --token native

# ERC-20 token (USDC — built-in)
afpay send --wallet evm-base --to <address> --amount 1000000 --token usdc
afpay receive --wallet evm-base --wait --amount 1000000 --token usdc

# Custom ERC-20 token (register first, then use by symbol)
afpay wallet config token-add --wallet evm-base --symbol dai --address 0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb --decimals 18
afpay send --wallet evm-base --to <address> --amount 1000000 --token dai
afpay receive --wallet evm-base --wait --amount 1000000 --token dai

afpay balance --network evm
```

### Bitcoin On-Chain

```bash
# Setup — Esplora backend (default)
afpay wallet create --network btc --btc-network signet --label btc-signet
afpay wallet create --network btc --btc-network mainnet --btc-address-type taproot --label btc-main

# Setup — Bitcoin Core RPC backend
afpay wallet create --network btc --btc-backend core-rpc --btc-core-url http://127.0.0.1:8332 --btc-core-auth-secret "user:pass" --label btc-core

# Setup — Electrum backend
afpay wallet create --network btc --btc-backend electrum --btc-electrum-url ssl://electrum.blockstream.info:60002 --label btc-electrum

# Receive address
afpay receive --network btc

# Optional: wait for incoming funds (exact amount match)
afpay receive --network btc --amount 1000 --wait --wait-timeout-s 120 --wait-poll-interval-ms 1000

# Send (amount in satoshis)
afpay send --network btc --to tb1q... --amount 5000

# Restore from mnemonic
afpay wallet create --network btc --btc-network signet --mnemonic-secret "word1 word2 ... word12"

# Custom Esplora endpoint
afpay wallet create --network btc --btc-network mainnet --btc-esplora-url https://my-esplora.example/api

afpay balance --network btc
afpay history status --transaction-id <txid>
```

BTC backend options are validated at wallet creation time:

- `--btc-backend core-rpc` requires `--btc-core-url`
- `--btc-backend electrum` requires `--btc-electrum-url`
- if provided, `--btc-esplora-url` must be non-empty
- the selected backend feature must be compiled in (`btc-esplora` / `btc-core` / `btc-electrum`)

When restoring from `--mnemonic-secret`, afpay runs one full chain scan and persists scan progress before returning.

### Cross-Network

```bash
afpay wallet list                    # All wallets
afpay balance                        # All balances
afpay balance --network sol          # Filter by network
afpay balance --wallet w_1a2b3c4d   # Filter by wallet
afpay history update                 # Incrementally sync backend/chain history to local store
afpay history update --network sol   # Sync one network
afpay history update --wallet <id>   # Sync one wallet
afpay history list                   # Query local history store
afpay history list --network sol     # Local filter by network
afpay history list --wallet <id>     # Local filter by wallet
afpay history status --transaction-id <id>
```

## Token Support

USDC and USDT are built-in for sol and evm. Other tokens (DAI, WBTC, BONK, WIF, JUP, etc.) can be registered per-wallet via `wallet config token-add`.

Balance queries automatically show all known tokens:

```json
{
  "confirmed": 500000,
  "unit": "lamports",
  "usdc_base_units": 1500000,
  "usdc_decimals": 6,
  "bonk_base_units": 250000,
  "bonk_decimals": 5
}
```

Built-in tokens: EVM — USDC/USDT on Base (8453), Arbitrum (42161), Ethereum (1). SOL — USDC/USDT on mainnet-beta, USDC on devnet. Raw contract/mint addresses can also be passed to `--token`.

## Spend Limits

Multi-tier spend limits — all rules checked before every send, any breach rejects the transaction:

```bash
afpay limit add --scope network --network cashu --window 1h --max-spend 10000
afpay limit add --scope network --network sol --token native --window 1h --max-spend 1000000
afpay limit add --scope wallet --wallet w_1a2b3c4d --window 24h --max-spend 50000
afpay limit add --scope global-usd-cents --window 24h --max-spend 500000   # requires exchange rate config
afpay limit remove --rule-id r_1a2b3c4d
afpay limit list
```

## Design Constraints

| Constraint | Approach |
|------------|----------|
| No unwrap/expect/panic | `#![deny(...)]` global lint |
| Key security | All secret fields use `_secret` suffix, agent-first-data auto-redacts |
| Spend limits non-bypassable | RPC daemon enforces limits server-side; agent cannot modify daemon config |
| Single-point failure isolation | Each network can run in its own VPS/container independently |
| Consistent output | All modes use the same Output types |
| RPC security | gRPC + PSK AES-256-GCM payload encryption, zero certificate management |
| Dual storage backend | redb (embedded) or PostgreSQL, selected via config |
| Pure Rust zero C deps | CDK, Alloy, BDK, Solana component crates, redb, sqlx, aes-gcm — all pure Rust |
| REST API (Docker-friendly) | HTTP `POST /v1/afpay` with Bearer auth, no client needed |
| Depends on agent-first-data | Output formatting, `_secret` redaction, OutputFormat enum |

## Docker / Podman

Single-container deployment with supervisord (afpay + optional phoenixd + optional bitcoind). Works with both Docker and Podman — all commands are interchangeable (`docker` ↔ `podman`, `docker compose` ↔ `podman compose`):

```bash
# REST mode (default) — curl-accessible
docker compose -f docker/docker-compose.yml up --build
podman compose -f docker/docker-compose.yml up --build   # equivalent

# RPC mode
AFPAY_MODE=rpc AFPAY_PORT=9400 docker compose -f docker/docker-compose.yml up --build

# MCP mode
AFPAY_MODE=mcp docker compose -f docker/docker-compose.yml up --build

# Podman without compose — build and run directly
podman build -t afpay -f docker/Dockerfile .
podman run -d --name afpay -p 9401:9401 \
  -v afpay-data:/data/afpay -v bitcoind-data:/data/bitcoind -v phoenixd-data:/data/phoenixd \
  -e AFPAY_MODE=rest afpay
```

`AFPAY_MODE` selects `rest`/`rpc`/`mcp`. Secrets auto-generated on first run and persisted to volumes. See [Architecture](docs/architecture.md) for full variable reference.

## Testing

```bash
cargo test
```

## Docs

- [Quick Start](docs/quickstart.md) — 30-second walkthrough for each network
- [Manual](docs/manual.md) — Full command reference, all run modes, protocol details
- [Architecture](docs/architecture.md) — Deployment patterns, RPC protocol, Provider design
- [Testing](docs/testing.md) — Unit and integration tests

## License

MIT
