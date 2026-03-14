# afpay Manual

Agent-first cryptocurrency micropayment tool. Single binary, multiple run modes, multi-network (Cashu, Lightning, Solana, EVM, Bitcoin on-chain).

## Run Modes

| Mode | Start | Use Case |
|------|-------|----------|
| cli | `afpay <subcommand>` | Single command, exits on completion |
| pipe | `afpay --mode pipe` | JSONL stdin/stdout long-lived connection |
| interactive | `afpay --mode interactive` | REPL + Tab completion + QR codes (requires `interactive` feature) |
| rpc | `afpay --mode rpc` | Daemon, gRPC + AES-256-GCM PSK encryption |
| rest | `afpay --mode rest` | HTTP REST API daemon, Bearer token auth (requires `rest` feature) |

## Global Options

```
--mode cli|pipe|interactive|rpc|rest  Run mode (default: cli)
--output json|yaml|plain    Output format (default: json)
--data-dir <path>           Data directory (default: ~/.afpay/)
--log <filters>             Log filters, comma-separated
--rpc-endpoint <host:port>  Connect to remote daemon (cli mode)
--rpc-listen <addr>         Listen address (rpc mode, default: 0.0.0.0:9400)
--rpc-secret <secret>       RPC encryption key
--rest-listen <addr>        Listen address (rest mode, default: 0.0.0.0:9401)
--rest-api-key <key>        API key for REST bearer authentication
```

## Token-Centric Amount Syntax

All send/receive commands use `--amount <value>` (always integer base units) with `--token <name>` identifying the asset:

```bash
# cashu/ln/btc — no --token needed, amount is sats
afpay send --network cashu --amount 100
afpay receive --network ln --amount 100       # BOLT11 invoice
afpay receive --network ln                    # BOLT12 offer (phoenixd only)
afpay send --network ln --to lno1... --amount 1000  # pay BOLT12 offer
afpay send --network btc --to <addr> --amount 5000

# sol/evm — --token required
afpay send --network sol --to <addr> --token native --amount 100000
afpay send --network sol --to <addr> --token usdc --amount 1500000
afpay send --network evm --to <addr> --token native --amount 1000000000
afpay send --network evm --to <addr> --token usdc --amount 1500000
```

## Multi-Level Configuration (config.toml)

For cascading RPC deployments, configure named `afpay_rpc` nodes and map networks to them in `config.toml`:

```toml
# Named afpay RPC nodes
[afpay_rpc.wallet-server]
endpoint = "10.0.1.5:9400"
endpoint_secret = "abc..."

[afpay_rpc.chain-server]
endpoint = "10.0.1.6:9400"
endpoint_secret = "def..."

# Network → afpay_rpc node name (omit = local)
[providers]
cashu = "wallet-server"
ln = "wallet-server"
sol = "chain-server"
evm = "chain-server"
btc = "chain-server"
```

Networks not listed in `[providers]` use their local implementation (if compiled in). Multiple networks can reference the same `afpay_rpc` node. For pure client mode (single remote daemon), use `--rpc-endpoint` / `--rpc-secret` CLI flags instead.

## Storage Backend (config.toml)

afpay supports two storage backends for wallet metadata, transaction history, and spend limit tracking. Select via `storage_backend` in `config.toml`:

```toml
# Embedded redb (default — no configuration needed)
storage_backend = "redb"

# PostgreSQL
storage_backend = "postgres"
postgres_url_secret = "postgres://user:pass@localhost/afpay"
```

| Backend | Config | Concurrency | Use Case |
|---------|--------|-------------|----------|
| `redb` | None (just works) | Single-process, file lock | Local CLI, single-agent setups |
| `postgres` | `postgres_url_secret` | Multi-process, advisory locks | Server deployments, multiple agents |

If `storage_backend` is omitted, defaults to `"redb"`.

PostgreSQL schema is auto-created on first connection. Spend limit enforcement uses `pg_advisory_xact_lock` to prevent concurrent reserve races.

When `storage_backend = "postgres"`, all data is stored in PostgreSQL — wallet metadata (including seed secrets), CDK proofs/keysets, transaction history, and spend accounting. When `storage_backend = "redb"`, everything is stored in local `.redb` files under `data_dir`.

## Supported Networks

| Network | Feature Flag | SDK | Status |
|---------|-------------|-----|--------|
| Cashu | `cashu` | CDK 0.15 | Implemented |
| Lightning | `ln-phoenixd` / `ln-lnbits` / `ln-nwc` | phoenixd (default) / LNbits / NWC | Implemented |
| Solana | `sol` | Local key + Solana JSON-RPC | Implemented (wallet/transfer/SPL Token) |
| EVM chain | `evm` | Alloy | Implemented (wallet/transfer/ERC-20 Token) |
| Bitcoin | `btc-esplora` / `btc-core` / `btc-electrum` | BDK | Implemented (SegWit/Taproot, mainnet/signet, multi-backend) |

Other features:

| Feature | Dependency | Description |
|---------|-----------|-------------|
| `redb` (default) | redb, fs2 | Embedded key-value storage |
| `postgres` (default) | sqlx | PostgreSQL storage backend |
| `interactive` | rustyline, qrcode | REPL interactive mode |
| `rest` | axum, tower-http | HTTP REST API server mode |
| `exchange-rate` (default) | reqwest | Exchange rate fetching for global-usd-cents limits |

Compile with feature flags:

```bash
cargo build --features cashu              # Cashu only (redb storage)
cargo build --features cashu,interactive  # Cashu + interactive mode
cargo build --no-default-features --features postgres,exchange-rate  # PostgreSQL-only server
cargo build --no-default-features         # Pure coordinator (no wallet SDK, no local storage)
```

---

## CLI Command Structure

CLI uses a unified `--network` structure. All send/receive/balance operations are top-level commands:

```
afpay send [--network <net>] ...    # Unified send
afpay receive [--network <net>] ... # Unified receive
afpay balance [--network <net>]     # Balance (all or filtered)
afpay balance --wallet <id>         # Filter by wallet
afpay wallet <subcommand>           # Wallet management
afpay history <subcommand>          # Transaction queries
afpay limit <subcommand>            # Spend limits
```

`--network` is optional when `--wallet` is specified — the network is inferred from wallet metadata:

```bash
afpay send --wallet sol-main --to <addr> --amount 1000 --token native
afpay receive --wallet cashu-main --amount 500
```

When neither `--wallet` nor `--network` is given for send/receive, an error is returned.

If only `--network` is given without `--wallet`, the wallet is auto-selected when exactly one wallet exists on that network; if multiple wallets exist, an error asks you to pass `--wallet`.

## Wallet Identification

All `--wallet` parameters accept either a wallet ID (`w_1a2b3c4d`) or a wallet label (`demo`). If a label matches multiple wallets, an error is returned asking you to use the wallet ID instead.

---

## Unified Send

Send tokens on any network.

```bash
afpay send --network <cashu|ln|sol|evm|btc> [options]
```

### Cashu Send

P2P cashu token (no `--to`) or withdraw to Lightning invoice (`--to <bolt11>`):

```bash
# P2P cashu token
afpay send --network cashu --amount 21 [--wallet <w>] [--onchain-memo <text>] [--cashu-mint <url> ...]

# Withdraw to Lightning invoice
afpay send --network cashu --to lnbc1... [--wallet <w>]
```

### Lightning Send

Pay a Lightning invoice (BOLT11) or BOLT12 offer:

```bash
# Pay BOLT11 invoice (amount encoded in invoice)
afpay send --network ln --to <bolt11_invoice> [--wallet <w>] [--local-memo <text>]

# Pay BOLT12 offer (phoenixd only, --amount required)
afpay send --network ln --to <bolt12_offer> --amount <sats> [--wallet <w>] [--local-memo <text>]
```

BOLT12 offers (`lno1...`) are only supported on the phoenixd backend. The `--amount` flag is required for offers (rejected for BOLT11 invoices, which encode the amount).

### Solana Send

```bash
afpay send --network sol --to <address> --token <native|usdc|usdt|mint_addr> --amount <u64> [--wallet <w>] [--onchain-memo <text>]
```

### EVM Send

```bash
afpay send --network evm --to <address> --token <native|usdc|usdt|contract_addr> --amount <u64> [--wallet <w>] [--onchain-memo <text>]
```

### Bitcoin Send

```bash
afpay send --network btc --to <address> --amount <sats> [--wallet <w>]
```

Amount is in satoshis. Addresses: `bc1p`/`bc1q` (mainnet) or `tb1p`/`tb1q` (signet).

### Address Validation

- **Sol**: base58, 32-44 chars, rejects `0x` prefix
- **EVM**: `0x` prefix + 40 hex chars
- **LN**: `lnbc`/`lntb`/`lnbcrt` prefix (BOLT11 invoice) or `lno1` prefix (BOLT12 offer)
- **BTC**: `bc1`/`tb1` prefix (bech32/bech32m)
- **--token**: rejects raw contract addresses; use `afpay wallet config token-add` to register first

### Send Response

```json
{
  "code": "sent",
  "wallet": "w_1a2b3c4d",
  "transaction_id": "tx_1700000000",
  "status": "confirmed",
  "token": "cashuBo2F...",
  "fee": {"value": 0, "token": "sats"}
}
```

For `--network ln`/`sol`/`evm`, the response code is `"paid"`:

```json
{
  "code": "paid",
  "wallet": "w_1a2b3c4d",
  "transaction_id": "5xK9...",
  "amount": {"value": 1000000, "token": "lamports"},
  "fee": {"value": 5000, "token": "lamports"},
  "trace": {"duration_ms": 2500}
}
```

---

## Unified Receive

Receive tokens on any network.

```bash
afpay receive --network <cashu|ln|sol|evm|btc> [options]
```

### Cashu Receive

```bash
# Receive a cashu token
afpay receive --cashu-token "cashuBo2F..." [--wallet <w>]

# Get deposit invoice (Lightning)
afpay receive --network cashu --amount <sats> [--wallet <w>] [--qr-svg-file]

# Claim tokens for a paid quote
afpay receive --network cashu --ln-quote-id <quote_id> --wallet <w>
```

### Lightning Receive

```bash
# BOLT11 invoice (one-time, amount-specific)
afpay receive --network ln --amount <sats> [--wallet <w>] [--qr-svg-file]

# BOLT12 offer (persistent, reusable — phoenixd only)
afpay receive --network ln [--wallet <w>]
```

When `--amount` is omitted, returns a reusable BOLT12 offer (`lno1...`) instead of a one-time BOLT11 invoice. BOLT12 offers are only supported on the phoenixd backend.

### Solana Receive

```bash
afpay receive --network sol [--wallet <w>] --token <native|usdc|symbol> [--wait [--onchain-memo <text> | --amount <n>]] [--wait-timeout-s <s>] [--wait-poll-interval-ms <ms>] [--min-confirmations <n>]
```

With `--wait`, must provide `--onchain-memo` or `--amount` to match incoming transactions. Use `--min-confirmations <n>` to require a minimum confirmation depth before considering the payment settled (e.g. `--min-confirmations 32` for finalized on Solana).

### EVM Receive

```bash
afpay receive --network evm [--wallet <w>] --token <native|usdc|symbol> [--wait --amount <n> [--onchain-memo <text>]] [--wait-timeout-s <s>] [--wait-poll-interval-ms <ms>] [--wait-sync-limit <n>] [--min-confirmations <n>]
```

Returns the wallet's receive address. With `--wait`, `--amount` is required and the command polls for matching incoming transactions. If `--onchain-memo` is provided, matching is limited to afpay-encoded EVM memos (`afpay:memo:v1:` payloads). The emitted `history_status.transaction_id` is an on-chain tx hash that can be queried again via `history status --transaction-id ...`. Use `--wait-sync-limit <n>` to control how many recent history records are scanned per poll while resolving the matched on-chain tx hash (default `500`, clamped to `1..5000`). Use `--min-confirmations <n>` to require a minimum confirmation depth (e.g. `--min-confirmations 12` for EVM L1).

### Bitcoin Receive

```bash
afpay receive --network btc [--wallet <w>] [--amount <sats>] [--wait [--wait-timeout-s <s>] [--wait-poll-interval-ms <ms>] [--wait-sync-limit <n>]]
```

Returns the next unused receive address. With `--wait`, polls wallet balance deltas via the configured backend (Esplora, Bitcoin Core RPC, or Electrum) and emits a `history_status` event when funds arrive. The emitted `history_status.transaction_id` is an on-chain BTC txid. Use `--wait-sync-limit <n>` to control how many recent history records are scanned per poll while resolving the matched txid (default `500`, clamped to `1..5000`).

If `--amount` is provided, only an incoming delta matching that amount is accepted.

---

## Wallet Commands

### wallet create

Create a wallet on a specific network:

```bash
afpay wallet create --network <cashu|ln|sol|evm|btc> [options]
```

| Network | Required Options |
|---------|-----------------|
| cashu | `--cashu-mint <url>` |
| ln | `--backend <nwc\|phoenixd\|lnbits>` + backend-specific options |
| sol | `--sol-rpc-endpoint <url>` |
| evm | `--evm-rpc-endpoint <url>` [--evm-chain-id <id>] |
| btc | [--btc-backend <esplora\|core-rpc\|electrum>] [--btc-network <mainnet\|signet>] [--btc-address-type <taproot\|segwit>] [--btc-esplora-url <url>] [--btc-core-url <url>] [--btc-core-auth-secret <user:pass>] [--btc-electrum-url <url>] |

BTC backend requirements (validated at create time):

- `--btc-backend core-rpc` requires `--btc-core-url`
- `--btc-backend electrum` requires `--btc-electrum-url`
- `--btc-esplora-url`, when provided, must be non-empty
- Chosen backend must be compiled in (`btc-esplora`, `btc-core`, or `btc-electrum`)

When creating a BTC wallet with `--mnemonic-secret` (restore flow), afpay performs one full chain scan and persists the resulting address/scan progress state.

Response:

```json
{
  "code": "wallet_created",
  "id": "cli_12345",
  "wallet": "w_1a2b3c4d",
  "network": "cashu",
  "address": "https://mint.example.com",
  "trace": {"duration_ms": 5}
}
```

### wallet list

List wallets across all networks.

```bash
afpay wallet list [--network <ln|sol|evm|cashu|btc>]
```

### wallet close

Close a zero-balance wallet (auto-detects network by wallet id).

```bash
afpay wallet close <wallet_id> [--dangerously-skip-balance-check-and-may-lose-money]
```

### wallet cashu-restore

Restore cashu proofs from mint (cashu-specific).

```bash
afpay wallet cashu-restore --wallet <wallet_id>
```

### wallet dangerously-show-seed

Display wallet mnemonic (local mode only).

```bash
afpay wallet dangerously-show-seed --wallet <wallet_id>
```

### wallet config show

View wallet's current configuration.

```bash
afpay wallet config show --wallet <wallet_id>
```

### wallet config set

Modify wallet settings.

```bash
afpay wallet config set --wallet <wallet_id> [--label <new_name>] [--rpc-endpoint <url>] [--chain-id <id>]
```

| Parameter | Description |
|-----------|-------------|
| `--label` | Change wallet label (any network) |
| `--rpc-endpoint` | Replace RPC endpoint (EVM/SOL only, repeatable) |
| `--chain-id` | Change chain ID (EVM only) |

### wallet config token-add

Register a custom token for balance queries (EVM/SOL wallets only).

```bash
afpay wallet config token-add --wallet <wallet_id> --symbol <sym> --address <contract_addr> [--decimals <n>]
```

### wallet config token-remove

Unregister a custom token.

```bash
afpay wallet config token-remove --wallet <wallet_id> --symbol <sym>
```

---

## Balance

Query balances.

```bash
afpay balance [--wallet <wallet_id>] [--network <cashu|ln|sol|evm|btc>] [--cashu-check]
```

- Omit `--wallet` and `--network` to query all wallet balances.
- `--network` filters to a specific network.
- `--cashu-check` verifies cashu proofs against mint (slower, more accurate).

Balance response includes native units and known token balances:

```json
{
  "confirmed": 500000,
  "pending": 0,
  "unit": "lamports",
  "usdc_base_units": 1500000,
  "usdc_decimals": 6
}
```

The `balance` field supports backend-specific categories; besides `confirmed` / `pending`, additional fields may appear (e.g. phoenixd's `fee_credit_sats`).

---

## History

### history list

Query transaction records from local store only.

```bash
afpay history list [--wallet <wallet_id>] [--network <cashu|ln|sol|evm|btc>] [--onchain-memo <text>] [--limit <n>] [--offset <n>] [--since-epoch-s <u64>] [--until-epoch-s <u64>]
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--network` | all | Filter by network |
| `--onchain-memo` | - | Filter by exact on-chain memo match |
| `--limit` | 20 | Number of records to return |
| `--offset` | 0 | Pagination offset |
| `--since-epoch-s` | - | Include records created at or after epoch second |
| `--until-epoch-s` | - | Include records created before epoch second |

`history list` does not pull chain/backend data. Run `history update` first to sync latest records into local store.

Each record includes an optional `fee` field with the actual transaction fee:

```json
{
  "transaction_id": "tx_...",
  "wallet": "w_1a2b3c4d",
  "network": "sol",
  "direction": "send",
  "amount": {"value": 1000000, "token": "lamports"},
  "fee": {"value": 5000, "token": "lamports"},
  "status": "confirmed",
  "created_at_epoch_s": 1700000000
}
```

Fee units per network: Cashu/LN/BTC = `sats`, Sol = `lamports`, EVM = `gwei`.

### history update

Incrementally sync provider history into local store.

```bash
afpay history update [--wallet <wallet_id>] [--network <cashu|ln|sol|evm|btc>] [--limit <n>]
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--wallet` | all wallets | Sync a single wallet |
| `--network` | all networks | Restrict sync scope |
| `--limit` | 200 | Max records scanned per wallet in this sync |

Response includes sync stats:

```json
{
  "code": "history_updated",
  "wallets_synced": 2,
  "records_scanned": 120,
  "records_added": 8,
  "records_updated": 11
}
```

### history status

Query single transaction status.

```bash
afpay history status --transaction-id <transaction_id>
```

For EVM transactions, when a transaction goes from pending to confirmed, `history status` automatically updates the precise fee from the receipt.

For BTC transactions with an on-chain txid, `history status` also queries current confirmation depth and backfills local `status` / `confirmed_at_epoch_s` when confirmation state changes.

---

## Spend Limits

### limit add

Add spend limit rules incrementally. Each rule gets a unique ID for targeted removal.

```bash
# Add a rule
afpay limit add --scope network --network cashu --window 1h --max-spend 10000
afpay limit add --scope global-usd-cents --window 24h --max-spend 50000

# Token-specific limits for sol/evm
afpay limit add --scope network --network evm --token usdc --window 24h --max-spend 100000000
afpay limit add --scope wallet --wallet demo --token native --window 1h --max-spend 100000

# Remove a rule by ID
afpay limit remove --rule-id r_1a2b3c4d
```

`--window` accepts `m` (minutes), `h` (hours), `d` (days) suffixes.

`--token` is optional. For sol/evm scoped limits, use `--token` to scope to a specific token (e.g. `native`, `usdc`). For cashu/ln limits, `--token` is not needed (network implies sats).

All rules are checked before every send — any breach rejects the transaction. Each node manages its own limits independently; `limit add` does not propagate to downstream nodes.

`global-usd-cents` scope rules require `exchange_rate` in `config.toml`. Built-in Kraken and CoinGecko public API sources with failover:

```toml
[exchange_rate]
ttl_s = 300

[[exchange_rate.sources]]
type = "kraken"
endpoint = "https://api.kraken.com"

[[exchange_rate.sources]]
type = "coingecko"
endpoint = "https://api.coingecko.com/api/v3"

[[exchange_rate.sources]]
type = "generic"
endpoint = "https://my-private-api.example/price"
api_key = "optional-api-key"
```

### limit list

Query current limit status. Also queries each downstream `afpay_rpc` node's limits recursively and assembles them into a tree:

```bash
afpay limit list
```

Response includes local limits and downstream node limits:

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

The `downstream` field is omitted when empty (no `afpay_rpc` nodes configured).

---

## Interactive Mode

`--mode interactive` provides a human-facing REPL interface. Requires `interactive` feature.

```bash
cargo build --features cashu,interactive
afpay --mode interactive
# Add --qr-svg-file to commands that support QR codes
```

### Explicit commands

All operations can be called with the network prefix:

```
afpay> cashu wallet create --cashu-mint https://mint.minibits.cash/Bitcoin --label demo
afpay> cashu wallet list
afpay> cashu send 21
afpay> cashu receive cashuBo2F...
afpay> cashu send-to-ln --to lnbc1...
afpay> cashu receive-from-ln 100
afpay> cashu balance
afpay> history list --wallet w_1a2b3c4d
```

### Shortcut commands

After setting an active wallet, common commands can omit the prefix and `--wallet` (cashu and ln each have different shortcuts):

```
afpay> use <wallet_id or label>
afpay(demo)> balance                   # = cashu balance --wallet <active>
afpay(demo)> receive-from-ln 100       # = cashu receive-from-ln --wallet <active> --amount 100
afpay(demo)> send 21                   # P2P cashu token (auto-select wallet)
afpay(demo)> receive cashuBo2F...      # Receive cashu token (auto-match wallet)
afpay(demo)> send-to-ln --to lnbc1...  # Withdraw to Lightning invoice
afpay(demo)> history                   # = history list --wallet <active>
afpay(demo)> receive-from-ln-claim <quote_id>  # Claim tokens
afpay(ln-main)> receive 100            # = ln receive --wallet <active> --amount 100
afpay(ln-main)> receive                # = ln receive --wallet <active> (BOLT12 offer, phoenixd only)
afpay(ln-main)> send --to lnbc1...     # = ln send --wallet <active> --to lnbc1...
afpay(ln-main)> send --to lno1... --amount-sats 500  # pay BOLT12 offer (phoenixd only)
afpay(ln-main)> invoice <transaction_id>  # Query invoice/payment_hash status
```

All commands can use `--wallet` to explicitly override the active wallet.

### Cross-network commands

```
afpay> wallet list                     # All network wallets
afpay> wallet list --network cashu     # Filter by network
afpay> balance                         # With active wallet → single balance; without → all
afpay> history list --wallet w_abc
afpay> history status --transaction-id tx_123
afpay> limit add --scope network --network cashu --window 1h --max-spend 10000
afpay> limit list
```

### Tab Completion

Press Tab for layered completion:

| Position | Completes |
|----------|-----------|
| 1st word | Command names (cashu, wallet, send, send-to-ln, balance, ...) |
| 2nd word after cashu | Cashu subcommands (send, receive, send-to-ln, receive-from-ln, wallet, ...) |
| After cashu wallet | create, close, list |
| After `--wallet` | Existing wallet IDs and labels |
| After `--network` | ln, sol, evm, cashu, btc |
| Starting with `-` | Available flags for current command |

### QR Codes

QR code files are not generated by default. Only when running `receive --qr-svg-file` will an SVG QR code file be generated (saved in `<data-dir>/qr-codes/`) and its path printed.

### Multi-Step Flows

**receive-from-ln guided flow:**

1. Run `receive-from-ln --qr-svg-file` → shows invoice + QR SVG file path
2. Prompt: "Pay the invoice above, then press Enter to claim (or type 'skip')..."
3. Press Enter → auto-claim, shows result
4. Type `skip` → prints `receive --network cashu --ln-quote-id` command for later use

**send confirmation:**

1. After parsing send parameters, shows summary (amount, destination, source wallet)
2. Prompt: "Confirm? [y/N]>"
3. `y` → execute send, `N`/other → cancel

### Session Commands

| Command | Description |
|---------|-------------|
| `use <wallet_id\|label>` | Set active wallet |
| `help` | Command help |
| `quit` / Ctrl-D | Exit |

Command history saved in `~/.afpay/.afpay_history`.

---

## Pipe Protocol

In `--mode pipe`, afpay reads JSONL from stdin (one request per line) and writes responses to stdout.

### Request Format

All requests are distinguished by the `code` field:

```json
{"code":"wallet_create","id":"req_1","network":"cashu","mint_url":"https://mint.minibits.cash/Bitcoin","label":"my-wallet"}
{"code":"ln_wallet_create","id":"req_2","backend":"nwc","nwc_uri_secret":"nostr+walletconnect://..."}
{"code":"wallet_list","id":"req_3"}
{"code":"wallet_close","id":"req_4","wallet":"w_1a2b3c4d"}
{"code":"balance","id":"req_5","wallet":"w_1a2b3c4d"}
{"code":"receive","id":"req_6","wallet":"w_1a2b3c4d","amount":{"value":100,"token":"sats"}}
{"code":"receive","id":"req_6b","wallet":"w_1a2b3c4d","network":"ln"}
{"code":"receive_claim","id":"req_7","wallet":"w_1a2b3c4d","quote_id":"abc123"}
{"code":"cashu_send","id":"req_8","amount":{"value":21,"token":"sats"}}
{"code":"cashu_receive","id":"req_9","token":"cashuBo2F..."}
{"code":"send","id":"req_10","to":"lnbc1...","network":"cashu"}
{"code":"restore","id":"req_11","wallet":"w_1a2b3c4d"}
{"code":"local_wallet_show_seed","id":"req_12","wallet":"w_1a2b3c4d"}
{"code":"history","id":"req_13","wallet":"w_1a2b3c4d","limit":20,"offset":0}
{"code":"history_status","id":"req_14","transaction_id":"tx_1700000000"}
{"code":"history_update","id":"req_15","transaction_id":"tx_1700000000","local_memo":{"note":"coffee"}}
{"code":"limit_add","id":"req_16","limit":{"scope":"network","network":"cashu","window_s":3600,"max_spend":10000}}
{"code":"limit_remove","id":"req_17","rule_id":"r_1a2b3c4d"}
{"code":"limit_list","id":"req_18"}
{"code":"limit_set","id":"req_19","limits":[{"scope":"network","network":"cashu","window_s":3600,"max_spend":10000}]}
{"code":"wallet_config_show","id":"req_20","wallet":"w_1a2b3c4d"}
{"code":"wallet_config_set","id":"req_21","wallet":"w_1a2b3c4d","label":"new-name"}
{"code":"wallet_config_token_add","id":"req_22","wallet":"w_1a2b3c4d","symbol":"dai","address":"0x6B17...","decimals":18}
{"code":"wallet_config_token_remove","id":"req_23","wallet":"w_1a2b3c4d","symbol":"dai"}
{"code":"config","log":["wallet","pay"],"limits":[{"scope":"network","network":"cashu","window_s":3600,"max_spend":10000}]}
{"code":"version"}
{"code":"close"}
```

### Concurrency

Requests are matched to responses via the `id` field. Non-system requests (not ping/config/close) execute asynchronously — multiple requests can be in-flight simultaneously.

### Shutdown

Send `{"code":"close"}` for graceful shutdown. afpay waits for all in-flight requests to complete (up to 5 second timeout), then outputs the `close` response and exits.

---

## REST Mode

`--mode rest` starts an HTTP server. Requests use the same `Input` JSON format as pipe mode (internally tagged with `"code"`). Responses are JSON arrays of `Output` objects. Requires `rest` feature.

### Endpoint

```
POST /v1/afpay
```

### Authentication

Every request must include one of:

- `Authorization: Bearer <api-key>`
- `X-API-Key: <api-key>`

### Request Format

Body is a JSON `Input` object (same as pipe protocol):

```bash
# Version
curl -X POST http://localhost:9401/v1/afpay \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{"code":"version"}'

# Balance
curl -X POST http://localhost:9401/v1/afpay \
  -H "Authorization: Bearer $KEY" \
  -d '{"code":"balance","id":"q1"}'

# Wallet list
curl -X POST http://localhost:9401/v1/afpay \
  -H "Authorization: Bearer $KEY" \
  -d '{"code":"wallet_list","id":"q2"}'

# Send
curl -X POST http://localhost:9401/v1/afpay \
  -H "Authorization: Bearer $KEY" \
  -d '{"code":"send","id":"q3","wallet":"w_1a2b3c4d","to":"lnbc1...","network":"cashu"}'
```

### Response Format

JSON array of `Output` objects (same schema as pipe mode):

```json
[{"code":"version","version":"0.1.0","trace":{"uptime_s":42,"requests_total":5,"in_flight":0}}]
```

### HTTP Status Codes

| Status | Meaning |
|--------|---------|
| 200 | Success |
| 400 | Invalid JSON or unknown input code |
| 401 | Missing or wrong API key |
| 403 | Local-only operation (e.g. limit_set, wallet_show_seed) |
| 422 | Operation returned an error output |

### Configuration

```bash
afpay --mode rest --rest-listen 0.0.0.0:9401 --rest-api-key "my-secret-key" --data-dir ~/.afpay
```

### Containers

The canonical container assets now live under `container/docker/`, and the macOS Apple Container CLI workflow lives under `container/apple-container/`. The image supports all three server modes via the `AFPAY_MODE` environment variable. All commands work with both Docker and Podman — replace `docker` with `podman`:

```bash
# REST mode (default) — curl-accessible, auto-generates API key
docker compose -f container/docker/compose.yaml up --build
podman compose -f container/docker/compose.yaml up --build   # equivalent

# macOS + Apple Container CLI workflow
./container/apple-container/up.sh

# RPC mode — for afpay CLI clients
AFPAY_MODE=rpc AFPAY_PORT=9400 docker compose -f container/docker/compose.yaml up --build
AFPAY_MODE=rpc AFPAY_PORT=9400 ./container/apple-container/up.sh
```

| Variable | Default | Description |
|----------|---------|-------------|
| `AFPAY_MODE` | `rest` | `rest` or `rpc` |
| `AFPAY_PORT` | `9401` | Listen port (rest/rpc) |
| `AFPAY_REST_API_KEY` | auto-generated | REST Bearer token |
| `AFPAY_RPC_SECRET` | auto-generated | RPC PSK secret |
| `ENABLE_PHOENIXD` | `true` | Start phoenixd |
| `ENABLE_BITCOIND` | `false` | Start bitcoind |
| `BTC_PRUNE_MB` | `550` | bitcoind prune target in MiB (`0` disables pruning) |

Secrets are auto-generated on first run and persisted to the data volume. Connection info is printed to stdout on startup. The auto-setup script (wallet creation for bitcoind/phoenixd) only runs in REST mode.

To enable the local bundled `bitcoind` in Docker/Podman compose, set both `ENABLE_BITCOIND=true` and `INSTALL_BITCOIND=true`. When enabled, it defaults to pruned `mainnet` mode.

Podman without compose:

```bash
podman build -t afpay -f container/docker/Dockerfile .
podman run -d --name afpay -p 9401:9401 \
  -v afpay-data:/data/afpay -v bitcoind-data:/data/bitcoind -v phoenixd-data:/data/phoenixd \
  -e AFPAY_MODE=rest afpay

podman exec -it afpay supervisorctl status
podman logs afpay
```

See `container/docker/` for the full setup with supervisord, phoenixd, and bitcoind, and `container/apple-container/` for the macOS Apple Container CLI launcher.

---

## Error Handling

All errors are returned via the `error` response:

```json
{
  "code": "error",
  "id": "req_1",
  "error_code": "wallet_not_found",
  "error": "wallet w_xxxxxxxx not found",
  "retryable": false,
  "trace": {"duration_ms": 1}
}
```

| error_code | Meaning | retryable |
|-----------|---------|-----------|
| `not_implemented` | Network not enabled | false |
| `wallet_not_found` | Wallet/transaction not found | false |
| `invalid_amount` | Invalid amount | false |
| `network_error` | Network/mint communication failure | true |
| `internal_error` | Internal error | false |

---

## Data Storage

Default directory `~/.afpay/`, structure (redb backend):

```
~/.afpay/
├── config.toml                         # runtime configuration (storage_backend, providers, etc.)
├── wallets-cashu/
│   ├── catalog.redb                    # cashu wallet index
│   └── w_1a2b3c4d/
│       ├── core.redb                   # wallet metadata + transaction_log
│       └── wallet-data/
│           └── cdk-wallet.redb         # CDK storage (proofs, keysets)
├── wallets-btc/
│   ├── catalog.redb                    # btc wallet index
│   └── w_55667788/
│       ├── core.redb                   # wallet metadata + transaction_log
│       └── wallet-data/
│           └── bdk_changeset.json      # BDK wallet state
├── wallets-ln-nwc/
│   ├── catalog.redb                    # ln-nwc wallet index
│   └── w_11223344/
│       ├── core.redb                   # wallet metadata + transaction_log
│       └── wallet-data/
├── spend/
│   ├── spend.redb                      # limit rules/reservations/accounting
│   └── exchange-rate-cache.redb        # exchange rate cache
└── .afpay_history                      # interactive mode command history
```

With `storage_backend = "postgres"`, only `config.toml` and `.afpay_history` remain on the local filesystem. All other data is stored in PostgreSQL.

With `storage_backend = "postgres"`, all data — wallet metadata (including seed secrets), CDK proofs/keysets, transaction history, and spend accounting — is stored in PostgreSQL instead of `.redb` files. The `wallet-data/` directories and `.redb` files are not used.

PostgreSQL tables (auto-created):

| Table | Contents |
|-------|----------|
| `wallets` | Wallet metadata (id, network, JSONB metadata) |
| `transactions` | Transaction history (JSONB records, indexed by wallet) |
| `spend_rules` | Spend limit rules (JSONB) |
| `spend_reservations` | Spend reservations (JSONB, BIGSERIAL id) |
| `spend_events` | Confirmed spend events (JSONB) |
| `exchange_rate_cache` | Exchange rate quotes (JSONB, keyed by pair) |

## Backup and Restore

For container deployments, the recovery-critical data is split across `afpay` and optional external backends:

- `/data/afpay` stores the embedded `redb` state by default, including wallet metadata, transaction history, spend limits, and recovery mnemonics for afpay-managed Cashu, BTC, Solana, and EVM wallets.
- LN wallets backed by `nwc` or `lnbits` do not have a mnemonic export; their backend credentials are stored inside `/data/afpay`.
- `phoenixd` is an external wallet. Back up `/data/phoenixd/.phoenix/` for `seed.dat`, `phoenix.conf`, and Phoenix database state.
- Local `bitcoind` data is optional for recovery and can usually be resynced. It is not included by default in the container backup scripts.

Container helper scripts:

```bash
# Apple container bind-mounted data
./container/apple-container/backup.sh
./container/apple-container/restore.sh /path/to/afpay-apple-backup.tar.gz

# Docker / Podman named volumes
CONTAINER_RUNTIME=docker ./container/docker/backup.sh
CONTAINER_RUNTIME=docker ./container/docker/restore.sh /path/to/afpay-docker-backup.tar.gz
```

Set `INCLUDE_BITCOIND=true` if you also want to archive the local `bitcoind` data.

If you use `storage_backend = "postgres"`, these scripts are not enough by themselves. You must also back up PostgreSQL because wallet metadata, seed secrets, transaction history, and spend accounting live in the database instead of local `.redb` files.

For mnemonic-based local wallets, you can also export recovery words directly from afpay (local mode only):

```bash
afpay --data-dir /data/afpay wallet dangerously-show-seed --wallet <wallet_id>
```

This works for afpay-managed Cashu, BTC, Solana, and EVM wallets. It does not export `phoenixd` seed material.

---

## Build and Test

### Build

```bash
cargo build                                 # debug (all networks by default)
cargo build --features cashu,interactive    # debug, Cashu + interactive only
cargo build --release                       # release
```

### Test

```bash
cargo test
```

Integration test details in [testing.md](testing.md).

### Lint

Project globally prohibits `unwrap`, `expect`, `panic`, `print_stderr`:

```rust
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
)]
```
