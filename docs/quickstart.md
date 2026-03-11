# afpay Quick Start

## Install

```bash
# Default build includes cashu + interactive + mcp
cargo install --path .
```

Binary: `afpay`

## 30-Second Walkthrough: Cashu

```bash
# 1. Create cashu wallet (--cashu-mint required)
afpay wallet create --network cashu --cashu-mint https://mint.minibits.cash/Bitcoin --label "demo"
# → returns wallet ID (e.g. w_1a2b3c4d) and mint address

# 2. Deposit — get Lightning invoice
afpay receive --wallet w_1a2b3c4d --amount 100
# → returns invoice and quote_id

# 3. Claim tokens (after paying the invoice)
afpay receive --wallet w_1a2b3c4d --ln-quote-id <quote_id>
# → returns claimed amount

# 4. Check balance
afpay balance --wallet w_1a2b3c4d
# → {"confirmed_sats": 100, "pending_sats": 0}

# 5. Send cashu token
afpay send --wallet w_1a2b3c4d --amount 21 --local-memo "coffee"
# → returns cashu token string (send to recipient)

# 6. Receive cashu token (recipient runs this)
afpay receive --cashu-token "cashuBo2F..."
# → 21 sats credited (auto-matches or creates wallet)

# 7. Transaction history
afpay history list --wallet w_1a2b3c4d
```

## 30-Second Walkthrough: Solana

```bash
# 1. Create Sol wallet (RPC endpoint required)
afpay wallet create --network sol --sol-rpc-endpoint https://api.mainnet-beta.solana.com --label sol-main

# 2. Check balance (lamports + USDC/USDT shown automatically)
afpay balance --network sol

# 3. Send native SOL
afpay send --network sol --to <address> --token native --amount 1000

# 4. Send USDC (1 USDC = 1000000 base units)
afpay send --network sol --to <address> --token usdc --amount 1000000

# 5. Check status
afpay history status --transaction-id <signature>
```

## 30-Second Walkthrough: EVM Chain

```bash
# 1. Create EVM wallet (Base mainnet)
afpay wallet create --network evm --evm-rpc-endpoint https://mainnet.base.org

# 2. Check balance (gwei + USDC/USDT shown automatically)
afpay balance --network evm

# 3. Send native ETH
afpay send --network evm --to <address> --token native --amount 1000000000000

# 4. Send USDC
afpay send --network evm --to <address> --token usdc --amount 1000000

# 5. Check status
afpay history status --transaction-id <tx_hash>
```

## 30-Second Walkthrough: Bitcoin On-Chain

```bash
# 1. Create BTC wallet (signet for testing, Esplora backend — default)
afpay wallet create --network btc --btc-network signet --label btc-test
# → returns wallet ID and taproot address (tb1p...)

# Or use Bitcoin Core RPC backend
afpay wallet create --network btc --btc-backend core-rpc --btc-core-url http://127.0.0.1:18443 --btc-core-auth-secret "user:pass" --btc-network signet --label btc-core

# Or use Electrum backend
afpay wallet create --network btc --btc-backend electrum --btc-electrum-url ssl://electrum.blockstream.info:60002 --btc-network signet --label btc-electrum

# 2. Check balance (satoshis)
afpay balance --network btc

# 3. Get receive address
afpay receive --network btc
# → returns current unused address

# Optional: wait for incoming funds (exact amount match)
afpay receive --network btc --amount 1000 --wait --wait-timeout-s 120 --wait-poll-interval-ms 1000

# 4. Send sats (after receiving funds from faucet or transfer)
afpay send --network btc --to <address> --amount 1000

# 5. Check status
afpay history status --transaction-id <txid>
```

Address types: `--btc-address-type taproot` (default, `tb1p`/`bc1p`) or `--btc-address-type segwit` (`tb1q`/`bc1q`).

Backend options: `--btc-backend esplora` (default), `--btc-backend core-rpc`, `--btc-backend electrum`. Each backend requires its own feature flag (`btc-esplora`, `btc-core`, `btc-electrum`).

Backend argument requirements are validated at `wallet create` time:

- `core-rpc` requires `--btc-core-url`
- `electrum` requires `--btc-electrum-url`
- `--btc-esplora-url` must be non-empty if provided

Mnemonic restore (`--mnemonic-secret`) performs one full chain scan immediately after wallet creation.

## Interactive Mode

```bash
afpay --mode interactive
# Add --qr-svg-file to commands that support QR codes
```

Workflow:

```
afpay> wallet create --network cashu --cashu-mint https://mint.minibits.cash/Bitcoin --label demo
afpay> use demo
afpay(demo)> receive-from-ln 64 --qr-svg-file  # shows invoice + QR SVG path → pay then press Enter to auto-claim
afpay(demo)> balance
afpay(demo)> send 16           # confirm then generates cashu token
afpay(demo)> receive cashuBo2F...
afpay(demo)> history
afpay(demo)> quit
```

Supports Tab completion (commands, subcommands, wallet ID/label, flags) and command history.

## Pipe Mode

For AI agents with long-lived connections:

```bash
afpay --mode pipe --output json
```

Send JSONL via stdin:

```json
{"code":"wallet_create","id":"req_1","network":"cashu","mint_url":"https://mint.minibits.cash/Bitcoin","label":"agent"}
{"code":"balance","id":"req_2","wallet":"w_1a2b3c4d"}
{"code":"version"}
{"code":"close"}
```

One request per line, stdout outputs corresponding responses. Supports concurrency — multiple requests can be in-flight, matched by `id` field.

## MCP Mode

```bash
afpay --mode mcp
```

Uses rmcp framework with MCP stdio transport. Exposes 32 tools covering all operations: `cashu_send`, `cashu_receive`, `wallet_create`, `balance`, `send`, `receive`, `history_list`, `limit_add`, etc. See [Manual](manual.md) for the full tool list.

## Spend Limits

```bash
# Network-level limit
afpay limit add --scope network --network cashu --window 1h --max-spend 10000

# Network-level limit with token
afpay limit add --scope network --network sol --token native --window 1h --max-spend 1000000

# Wallet-level limit
afpay limit add --scope wallet --wallet w_1a2b3c4d --window 24h --max-spend 200000

# Global USD-cents limit (requires exchange_rate in config.toml)
afpay limit add --scope global-usd-cents --window 24h --max-spend 50000

# Remove a rule by ID
afpay limit remove --rule-id r_1a2b3c4d

# Check current status
afpay limit list
```

## Output Formats

```bash
afpay --output json   balance --wallet w_1a2b3c4d   # default
afpay --output yaml   balance --wallet w_1a2b3c4d
afpay --output plain  balance --wallet w_1a2b3c4d
```

## Data Directory

Default `~/.afpay/`, override with `--data-dir`:

```bash
afpay --data-dir /tmp/afpay wallet create --network cashu --cashu-mint https://mint.minibits.cash/Bitcoin
```

## Next Steps

Full command reference and protocol details in the [Manual](manual.md).
