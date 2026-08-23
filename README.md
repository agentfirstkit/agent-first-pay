# Agent-First Pay

A payment tool for AI agents — send and receive across five networks through one interface, with spending limits you control.

> **Ask your agent:** "Pay this $20 invoice from my Lightning balance."

## The problem: five networks, five tools, and money to lose

Agents are starting to handle real money: paying for an API call, settling a bill, tipping a service. But every payment network — Cashu, Lightning, Solana, Ethereum-style chains, Bitcoin — has its own tools, its own quirks, its own way of saying "done" or "failed". An agent should not have to learn five of them.

And money is dangerous to automate. A bug, a bad prompt, or a confused agent should not be able to drain a wallet — but most payment tools assume a careful human is the one pressing the button.

## What it does: one interface across five networks, with hard spending limits

Agent-First Pay gives an agent one way to move money across all five networks — and puts hard spending limits in front of every payment, enforceable somewhere the agent cannot reach.

- **Five networks, one interface.** Cashu, Lightning, Solana, EVM chains, and on-chain Bitcoin — the same commands for all of them.
- **Spending limits that hold.** Per-wallet, per-network, and global caps, checked before every send. Run the limits on a separate machine and the agent cannot change them.
- **Built for agents.** Every result comes back as structured data; secrets are hidden automatically.
- **Runs how you need it.** As a one-shot command, a long-lived session, an interactive terminal app, or a remote server.
- **One safe binary.** Pure Rust with no C dependencies; compile in only the networks you actually use.

## Where to use it: paying for services, capping spend, and accepting funds

- **An agent paying for services** — API credits, compute, data — across whichever network a vendor accepts.
- **Capping what an agent can spend** — set a daily limit and let the agent operate freely under it.
- **A shared payment daemon** — run afpay on a trusted machine; agents send requests, limits stay enforced server-side. `afpay container install` stands one up in a container (Docker, Podman, or Apple) with one command.
- **Accepting payments** — generate invoices and watch for incoming funds on any of the five networks.

## Adopt it: hand afpay to your agent

The quickest way to find out whether afpay is worth it is to let your agent read
it and tell you. Paste this to your agent:

> Read what Agent-First Pay is at https://agentfirstkit.com/agent-first-pay,
> then tell me in plain terms what it would do for me and whether it fits what
> I'm working on. If it's a fit, install it — the prebuilt package for the quick
> path, or build from source after a quick security review of the repo if you'd
> rather read what you run — then run `afpay skill install` so you follow its
> behavior rules.

If it's a fit, install it — a prebuilt package, or from source if you want to
read it first:

```bash
# prebuilt binary
brew install agentfirstkit/tap/afpay   # macOS / Linux
scoop bucket add agentfirstkit https://github.com/agentfirstkit/scoop-bucket && scoop install afpay   # Windows
cargo install agent-first-pay           # any platform (from crates.io)

# or build from source after reviewing the repo
git clone https://github.com/agentfirstkit/agent-first-pay
cargo install --path agent-first-pay
```

The binary is `afpay`.

Then install the embedded [Agent Skill](skills/agent-first-pay/SKILL.md) so the agent
follows afpay's behavior rules — staying under the spend limits, never
double-paying, reading the structured output. `skill install` targets Codex,
Claude Code, and opencode; `skill status` reports whether each install is
present, valid, and current:

```bash
afpay skill install
afpay skill status
```

## Architecture

All network wallets are managed in one process via the Provider trait:

```
afpay <command>
  ├── cashu provider
  ├── ln provider
  ├── sol provider
  ├── evm provider
  └── btc provider
```

For remote operation there is exactly one machine face — the HTTP resource API —
and everything talks to it, including afpay itself:

```
curl / scripts / any language        another afpay node
  │ GET /v1/wallets                    │ --peer-url http://host:9401
  │ POST /v1/send-plans → /v1/sends     │ (the same routes, same bearer token)
  └──────────────→ afpay --mode rest (VPS / Docker) ←──────────────┘
```

The HTTP face describes itself: `GET /openapi.json` and `GET /schemas/index.json`
need no credential, and `afpay api export` writes the same contract without a
running daemon.

Federation is not a second protocol. `<command> --peer-url` runs the command on
another afpay node over the routes above, so a peer can only ask for what any
agent holding that node's token could ask for — and every node it forwards
through enforces its own spend limits.

See [Architecture](docs/architecture.md) for advanced multi-server deployment patterns.

## Supported Networks

| Network | Unit | Token Support | Feature |
|---------|------|---------------|---------|
| Cashu | sats | — | `cashu` |
| Lightning | sats | — | `ln-phoenixd` (default) / `ln-lnbits` / `ln-nwc` |
| Solana | lamports | USDC, USDT (SPL) | `sol` |
| EVM chain | gwei | USDC, USDT (ERC-20) | `evm` |
| Bitcoin | sats | — | `btc-esplora` / `btc-core` / `btc-electrum` |

Default builds include the HTTP API and federation, redb + PostgreSQL, Cashu, Phoenixd Lightning, Solana, EVM, Bitcoin Esplora, exchange rates, interactive UI, and backup support. Selective compilation:

```bash
cargo build --features cashu              # Cashu only (minimal binary)
cargo build --features cashu,ln           # Cashu + Lightning
cargo build --features btc-esplora        # Bitcoin on-chain (Esplora backend)
cargo build --features btc-core           # Bitcoin on-chain (Bitcoin Core RPC)
cargo build --features btc-electrum       # Bitcoin on-chain (Electrum)
cargo build --no-default-features --features federation  # Coordinator only (no wallet SDK)
cargo build                               # Default production feature set
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

## Usage

Default mode is CLI — one command per invocation:

```bash
# Local (wallet on this machine)
afpay balance

# On another afpay node (forwarded over its HTTP API)
afpay balance --peer-url http://10.0.1.5:9401 --peer-api-key-secret "64-char-hex"
```

Other modes: `--mode interactive` (REPL), `--mode tui` (full-screen terminal UI), `--mode pipe` (JSONL stdin/stdout), `--mode rest` (the HTTP API). See [CLI Reference](docs/cli.md) for flags and subcommands, and [Architecture](docs/architecture.md) for deployment and protocol details.

## Modes

| Mode | Start | Use Case |
|------|-------|----------|
| cli | `afpay <subcommand>` | One command, local or forwarded to a peer via `--peer-url` |
| pipe | `afpay --mode pipe` | Long-lived JSONL stdin/stdout session for agents |
| interactive | `afpay --mode interactive` | Human REPL with completion and QR helpers |
| tui | `afpay --mode tui` | Full-screen terminal workflow over the same interactive command interface |
| rest | `afpay --mode rest` | The HTTP resource API — for curl, containers, general clients, and other afpay nodes; listens on `127.0.0.1:9401` by default |

### Reaching a daemon that is not on this machine

`--mode rest` binds to `127.0.0.1:9401`. Binding anywhere else needs
`--public-listen`, which is an acknowledgement, not a security feature: afpay
serves plain HTTP and **does not terminate TLS**. Put one of the three
arrangements below between the network and the daemon.

**Encryption is not authentication.** All three carry the bearer token as well:
the tunnel decides who can reach the port, the token decides who may spend. Never
drop `--rest-api-key-secret` because the transport is private.

**1. Tailscale or WireGuard — best fit for your own machines.** Encryption plus
device identity, no certificates to issue or rotate. Bind afpay to the tunnel
interface only:

```bash
# on the daemon host
afpay --mode rest --rest-listen 100.64.0.7:9401 --public-listen \
  --rest-api-key-secret "$(openssl rand -hex 32)"

# from any other device on the tailnet
afpay balance --peer-url http://100.64.0.7:9401 --peer-api-key-secret "…"
```

**2. SSH tunnel — the safest posture available.** afpay keeps listening only on
loopback, so nothing is exposed even if the firewall is wrong. No
`--public-listen` at all:

```bash
# on the daemon host: the default bind is already correct
afpay --mode rest --rest-api-key-secret "$(openssl rand -hex 32)"

# on the client: forward the loopback port over SSH
ssh -N -L 9401:127.0.0.1:9401 user@daemon-host &
afpay balance --peer-url http://127.0.0.1:9401 --peer-api-key-secret "…"
```

**3. TLS reverse proxy — when you want a stable hostname.** Caddy's internal CA
issues and renews LAN certificates automatically; trust its root once per client
(`caddy trust`):

```caddyfile
# Caddyfile on the daemon host
afpay.home.arpa {
    tls internal
    reverse_proxy 127.0.0.1:9401
}
```

```bash
afpay --mode rest --rest-api-key-secret "$(openssl rand -hex 32)"   # loopback only
afpay balance --peer-url https://afpay.home.arpa --peer-api-key-secret "…"
```

**Why afpay does not do TLS itself.** No public CA will issue for a LAN name, so
built-in TLS would mean shipping a private-CA workflow — exactly what Caddy and
Tailscale already do better. Certificate loading, renewal, and SNI would have to
be duplicated across every listener rather than solved once in front of them. And
TLS would not answer the question that actually matters here: it proves the
server's name, not that the caller may move money. That is the bearer token's
job, and it is required either way.

The HTTP face is narrower than the CLI by design. Reading a seed, editing spend-limit rules, repairing a reservation and changing daemon config are local operations: they have no route, so a leaked bearer token cannot raise its own spending limit. Federation goes through the same routes, so a peer cannot reach them either.

## Quick Start

Every `send` below resolves a plan and returns a `plan_id`; `afpay pay confirm
--plan-id …` is what actually pays. See [Paying is two steps](#paying-is-two-steps).

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
afpay wallet create --network ln --backend phoenixd --endpoint-url http://localhost:9740 --password-secret "hunter2"
afpay wallet create --network ln --backend lnbits --endpoint-url https://legend.lnbits.com --admin-key-secret "abc123"

# Receive — BOLT11 invoice (one-time, amount-specific)
afpay receive --network ln --amount 500

# Receive — BOLT12 offer (persistent, reusable — phoenixd only)
afpay receive --network ln

# Send — pay BOLT11 invoice
afpay send --network ln --to lnbc1...

# Send — pay BOLT12 offer (phoenixd only, --amount required)
afpay send --network ln --to lno1... --amount 1000

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
afpay sol config --wallet sol-main token-add --symbol bonk --address DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263 --decimals 5
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

# Optional: match afpay-encoded on-chain memo while waiting (amount is still required)
afpay receive --wallet evm-base --wait --amount 1000000 --token usdc --onchain-memo "order:abc"

# Optional: increase per-poll history scan window when waiting (default 500, clamp 1..5000)
afpay receive --wallet evm-base --wait --amount 1000000 --token usdc --wait-sync-limit 1500

# Custom ERC-20 token (register first, then use by symbol)
afpay evm config --wallet evm-base token-add --symbol dai --address 0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb --decimals 18
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

# Optional: increase per-poll history scan window when waiting (default 500, clamp 1..5000)
afpay receive --network btc --amount 1000 --wait --wait-sync-limit 1500

# Send (amount in satoshis)
afpay send --network btc --to tb1q... --amount 5000

# Restore from mnemonic
afpay wallet create --network btc --btc-network signet --mnemonic-secret "word1 word2 ... word12"

# Custom Esplora endpoint
afpay wallet create --network btc --btc-network mainnet --btc-esplora-url https://my-esplora.example/api

afpay balance --network btc
afpay history status --transaction-id <txid>
```

`receive --wait` for EVM/BTC emits on-chain transaction IDs in `history_status.transaction_id`, so they can be re-queried with `history status --transaction-id ...`.

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

USDC and USDT are built-in for sol and evm. Other tokens (DAI, WBTC, BONK, WIF, JUP, etc.) can be registered per-wallet via `<network> config --wallet <id> token-add`.

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

## Paying is two steps

`send` resolves a payment; it does not make one. It reports the wallet afpay
would use, what leaves it, the fee it expects, and the spend budgets it would
debit, under a single-use `plan_id`. Confirming that id is what pays.

```bash
afpay ln send --to lnbc1...
# → {"code":"pay_planned","plan_id":"plan_…","wallet":"w_…","amount_native":250000,
#    "fee_estimate_native":2500,"fee_unit":"sats","spend_debits":[…]}

afpay pay confirm --plan-id plan_… --idempotency-key pay-invoice-1
```

Every caller goes through the same two steps — the CLI, the pipe, the HTTP API
(`POST /v1/send-plans` then `POST /v1/sends`), the confirmation window, and one
afpay node paying through another. A plan expires after 15 minutes, is
confirmable exactly once, and is refused outright if the workspace, daemon
configuration, wallet, or spend rules changed after it was resolved. What was
reviewed and what happens cannot be two different payments.

## Spend Limits

Multi-tier spend limits — all rules checked before every send, any breach rejects the transaction:

```bash
afpay cashu limit add --window 1h --max-spend 10000
afpay sol limit add --token native --window 1h --max-spend 1000000
afpay cashu limit --wallet w_1a2b3c4d add --window 24h --max-spend 50000
afpay global limit add --window 24h --max-spend 500000   # requires exchange rate config
afpay limit remove --rule-id r_1a2b3c4d
afpay limit list
```

## Design Constraints

| Constraint | Approach |
|------------|----------|
| No unwrap/expect/panic | `#![deny(...)]` global lint |
| Key security | All secret fields use `_secret` suffix, agent-first-data auto-redacts |
| Spend limits non-bypassable | The daemon enforces limits server-side; agent cannot modify daemon config |
| Nothing pays without review | Money leaves a wallet only by confirming a plan afpay resolved and recorded. There is no operation, on any transport, that takes a description of a payment and makes it in one step |
| Single-point failure isolation | Each network can run in its own VPS/container independently |
| Consistent output | All modes use the same Output types |
| One machine face | Every non-human caller — curl, containers, other afpay nodes — speaks the same `/v1` HTTP routes with the same Bearer token; there is no second protocol to keep in sync or to widen for federation |
| Dual storage backend | redb (embedded) or PostgreSQL, selected via config |
| Pure Rust zero C deps | CDK, Alloy, BDK, Solana component crates, redb, sqlx — all pure Rust |
| HTTP API (Docker-friendly) | Resource routes under `/v1` with Bearer auth and an OpenAPI 3.2 contract; every operation a retry could duplicate takes an `Idempotency-Key`; key material, spend-limit rules and daemon config have no route at all; reach it off-box through Tailscale/WireGuard, an SSH tunnel, or a TLS reverse proxy (see [Reaching a daemon that is not on this machine](#reaching-a-daemon-that-is-not-on-this-machine)) |
| Depends on agent-first-data | Output formatting, `_secret` redaction, OutputFormat enum |

## Containers

Single-container deployment with supervisord (afpay + optional phoenixd + optional bitcoind). The one-command path is `afpay container install` — it builds the image from a recipe embedded in the binary and runs the daemon under **Docker, Podman, or Apple `container`** (auto-detected; override with `--runtime`):

```bash
# --allow seeds the operator allowlist afpay requires to expose a listener
# (categories: mint, esplora, sol-rpc, evm-rpc, btc-core, btc-electrum, ln;
# repeatable).
afpay container install --allow mint=https://mint.example

# Credentials and the credential-bearing client command are redacted by
# default. Reveal them only for an operator who is ready to store them safely.
afpay container status --reveal-daemon-secret

# Bundle + enable phoenixd (or bitcoind)
afpay container install --with phoenixd --allow ln=http://127.0.0.1:9740

# Status / logs / teardown
afpay container status        # endpoint + redacted credential fields
afpay container logs -f
afpay container uninstall --purge
```

`install` downloads the matching prebuilt release (the Dockerfile's `downloader`
stage) — no source tree, no Rust toolchain in the image. Pass `--from-source` to
compile the `builder` stage from a checkout instead (dev / unreleased versions);
under Apple `container` that needs a resized builder VM — see
[container/README.md](container/README.md).

From a source checkout you can also drive the runtime CLI directly (all commands
are interchangeable, `docker` ↔ `podman`):

```bash
docker compose -f container/docker/compose.yaml up --build
podman build -t afpay -f container/docker/Dockerfile .
```

The bearer API key is auto-generated on first run, persisted to the data volume with private permissions, and passed through an environment variable rather than a process argument. `bitcoind` is disabled by default; when enabled it runs pruned `mainnet` with `BTC_PRUNE_MB=550`. See [container/README.md](container/README.md) for backup/restore and the full container workflow, and [Architecture](docs/architecture.md) for the variable reference.

## Data and Recovery

- Default local data dir is `~/.afpay/`.
- `storage_backend = "redb"` keeps wallet metadata, spend limits, and history in local `.redb` files under private `0700` directories and `0600` database files on Unix.
- `storage_backend = "postgres"` moves wallet metadata, seed secrets, transaction history, and spend accounting into PostgreSQL.
- For container deployments, back up `/data/afpay` plus `/data/phoenixd/.phoenix/` when using phoenixd.
- If you use PostgreSQL storage, back up PostgreSQL as well; volume backups alone are not enough.
- Local wallets with mnemonics can be exported with `afpay wallet dangerously-show-seed --wallet <wallet_id>`.

## Testing

```bash
cargo test
```

## Docs

- [CLI Reference](docs/cli.md) — every command and flag
- [Architecture](docs/architecture.md) — how it is built, deployment patterns, the HTTP API, Provider design
- [Testing](docs/testing.md) — unit and integration tests

## License

MIT
