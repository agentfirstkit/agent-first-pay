<!-- Generated. Do not edit by hand. Regenerate: afpay --help --recursive --output markdown -->

# afpay CLI Reference

# Agent-First Pay - A payment tool for AI agents — send and receive across five networks through one interface, with spending limits you control.

```text
Usage: afpay [OPTIONS] [COMMAND]

Commands:
  global     Global (cross-network) operations
  cashu      Cashu operations
  ln         Lightning Network operations (NWC, phoenixd, LNbits)
  sol        Solana operations
  evm        EVM chain operations (Base, Arbitrum)
  btc        Bitcoin on-chain operations
  wallet     List all wallets (cross-network)
  balance    All wallets balance (cross-network)
  history    History queries
  limit      Spend limit list and remove (cross-network)
  skill      Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode, Hermes)
  container  Build and run the afpay daemon container (Docker, Podman, or Apple) from the embedded recipe
  help       Print this message or the help of the given subcommand(s)

Options:
      --mode <MODE>
          Run mode

          [default: cli]
          [possible values: cli, pipe, interactive, tui, rpc, rest]

      --rpc-endpoint <RPC_ENDPOINT>
          Connect to remote RPC daemon (cli mode)

      --rpc-listen <RPC_LISTEN>
          Listen address for RPC daemon (rpc mode)

          [default: 127.0.0.1:9400]

      --rpc-secret <RPC_SECRET>
          RPC encryption secret

      --rest-listen <REST_LISTEN>
          Listen address for REST HTTP server (rest mode)

          [default: 127.0.0.1:9401]

      --rest-api-key <REST_API_KEY>
          API key for REST bearer authentication (rest mode)

      --public-listen
          Allow binding REST/RPC to non-loopback addresses; use only behind TLS/firewall

      --data-dir <DATA_DIR>
          Wallet and data directory

      --output <OUTPUT>
          Output format

          [default: json]

      --stdout-file <PATH>
          Redirect stdout bytes to this file

      --stderr-file <PATH>
          Redirect stderr bytes to this file

      --log <LOG>
          Log filters (comma-separated)

      --dry-run
          Preview the command without executing it

  -h, --help
          Print help. Add --recursive to expand every nested subcommand; add --output json|yaml|markdown to render this help in another format.

  -V, --version
          Print version
```

## Agent-First Pay global - Global (cross-network) operations

```text
Usage: global <COMMAND>

Commands:
  limit    Global spend limit (USD cents)
  config   Global runtime configuration
  backup   Back up all data to a .tar.zst archive
  restore  Restore all data from a .tar.zst archive
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay global limit - Global spend limit (USD cents)

```text
Usage: limit <COMMAND>

Commands:
  add   Add a global spend limit (USD cents)
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay global limit add - Add a global spend limit (USD cents)

```text
Usage: add --window <WINDOW> --max-spend <MAX_SPEND>

Options:
      --window <WINDOW>
          Time window: e.g. 30m, 1h, 24h, 7d

      --max-spend <MAX_SPEND>
          Maximum spend in USD cents

  -h, --help
          Print help
```

### Agent-First Pay global config - Global runtime configuration

```text
Usage: config <COMMAND>

Commands:
  get   Get a config value by dot-path key (omit key to show all)
  set   Set a config value by dot-path key
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay global config get - Get a config value by dot-path key (omit key to show all)

```text
Usage: get [KEY]

Arguments:
  [KEY]
          Dot-path key (e.g. log, exchange_rate.ttl_s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay global config set - Set a config value by dot-path key

```text
Usage: set <KEY> [VALUES]...

Arguments:
  <KEY>
          Dot-path key (e.g. log, exchange_rate.ttl_s)

  [VALUES]...
          Value(s) to set

Options:
  -h, --help
          Print help
```

### Agent-First Pay global backup - Back up all data to a .tar.zst archive

```text
Usage: backup [OPTIONS]

Options:
      --output <OUTPUT>
          Output archive path (default: ./afpay-global-{timestamp}.tar.zst)

      --extra-dir <EXTRA_DIR>
          Include an extra directory: --extra-dir label=/path (repeatable)

  -h, --help
          Print help
```

### Agent-First Pay global restore - Restore all data from a .tar.zst archive

```text
Usage: restore [OPTIONS] <ARCHIVE>

Arguments:
  <ARCHIVE>
          Path to the backup archive

Options:
      --dangerously-overwrite
          Clear all existing data before restoring (default: merge)

      --pg-url-secret <PG_URL_SECRET>
          Override PostgreSQL connection URL for the pg restore step

      --extra-dir <EXTRA_DIR>
          Restore an extra directory: --extra-dir label=/path (repeatable)

  -h, --help
          Print help
```

## Agent-First Pay cashu - Cashu operations

```text
Usage: cashu <COMMAND>

Commands:
  send                   Send P2P cashu token (outputs token string; for Lightning, use send-to-ln)
  receive                Receive cashu token
  send-to-ln             Send cashu to a Lightning invoice
  receive-from-ln        Create Lightning invoice to receive cashu from LN
  receive-from-ln-claim  Claim minted tokens from a receive-from-ln quote
  balance                Check cashu balance
  wallet                 Wallet management
  limit                  Spend limit for cashu network or a specific cashu wallet
  config                 Per-wallet configuration
  backup                 Back up cashu wallet data to a .tar.zst archive
  restore                Restore cashu wallet data from a .tar.zst archive
  help                   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay cashu send - Send P2P cashu token (outputs token string; for Lightning, use send-to-ln)

```text
Usage: send [OPTIONS] --amount-sats <AMOUNT_SATS>

Options:
      --amount-sats <AMOUNT_SATS>
          Amount in sats (base units)

      --cashu-mint <MINT_URL>
          Restrict to wallets on these mint URLs (tried in order)

      --wallet <WALLET>
          Source wallet ID (auto-selected if omitted)

      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo (sent with the transaction)

      --local-memo <LOCAL_MEMO>
          Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)

      --idempotency-key <IDEMPOTENCY_KEY>
          Opaque idempotency key (≤128 chars). A second send with the same key and identical body replays the first response instead of re-broadcasting; a different body returns idempotency_conflict. Persisted for 24h

  -h, --help
          Print help
```

### Agent-First Pay cashu receive - Receive cashu token

```text
Usage: receive [OPTIONS] <TOKEN>

Arguments:
  <TOKEN>
          Cashu token string

Options:
      --wallet <WALLET>
          Wallet ID (auto-matched from token if omitted)

  -h, --help
          Print help
```

### Agent-First Pay cashu send-to-ln - Send cashu to a Lightning invoice

```text
Usage: send-to-ln [OPTIONS] --to <TO>

Options:
      --to <TO>
          Lightning invoice (bolt11)

      --wallet <WALLET>
          Source wallet ID (auto-selected if omitted)

      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo (sent with the transaction)

      --local-memo <LOCAL_MEMO>
          Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)

      --idempotency-key <IDEMPOTENCY_KEY>
          Opaque idempotency key (≤128 chars). A second send with the same key and identical body replays the first response instead of re-broadcasting; a different body returns idempotency_conflict. Persisted for 24h

  -h, --help
          Print help
```

### Agent-First Pay cashu receive-from-ln - Create Lightning invoice to receive cashu from LN

```text
Usage: receive-from-ln [OPTIONS]

Options:
      --amount-sats <AMOUNT_SATS>
          Amount in sats (base units)

      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo (sent with the transaction)

      --wallet <WALLET>
          Wallet ID (auto-selected if omitted)

      --wait
          Wait for payment / matching receive transaction

      --wait-timeout-s <WAIT_TIMEOUT_S>
          Timeout in seconds for --wait

      --wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>
          Poll interval in milliseconds for --wait

      --qr-svg-file
          Write receive QR payload to an SVG file

  -h, --help
          Print help
```

### Agent-First Pay cashu receive-from-ln-claim - Claim minted tokens from a receive-from-ln quote

```text
Usage: receive-from-ln-claim --wallet <WALLET> --ln-quote-id <LN_QUOTE_ID>

Options:
      --wallet <WALLET>
          Wallet ID

      --ln-quote-id <LN_QUOTE_ID>
          Quote ID / payment hash from deposit

  -h, --help
          Print help
```

### Agent-First Pay cashu balance - Check cashu balance

```text
Usage: balance [OPTIONS]

Options:
      --wallet <WALLET>
          Wallet ID (omit to show all cashu wallets)

      --check
          Verify proofs against mint (slower but accurate)

  -h, --help
          Print help
```

### Agent-First Pay cashu wallet - Wallet management

```text
Usage: wallet <COMMAND>

Commands:
  create                 Create a new cashu wallet
  close                  Close a zero-balance cashu wallet
  list                   List cashu wallets
  dangerously-show-seed  Dangerously show wallet seed mnemonic (12 BIP39 words)
  restore                Restore lost proofs from mint (fixes counter/proof sync issues)
  help                   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay cashu wallet create - Create a new cashu wallet

```text
Usage: create [OPTIONS] --cashu-mint <MINT_URL>

Options:
      --cashu-mint <MINT_URL>
          Cashu mint URL

      --label <LABEL>
          Optional label

      --mnemonic-secret <MNEMONIC_SECRET>
          Existing BIP39 mnemonic secret to restore this wallet

  -h, --help
          Print help
```

#### Agent-First Pay cashu wallet close - Close a zero-balance cashu wallet

```text
Usage: close [OPTIONS] --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

      --dangerously-skip-balance-check-and-may-lose-money
          Dangerously skip balance checks when closing wallet

  -h, --help
          Print help
```

#### Agent-First Pay cashu wallet list - List cashu wallets

```text
Usage: list

Options:
  -h, --help
          Print help
```

#### Agent-First Pay cashu wallet dangerously-show-seed - Dangerously show wallet seed mnemonic (12 BIP39 words)

```text
Usage: dangerously-show-seed --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

#### Agent-First Pay cashu wallet restore - Restore lost proofs from mint (fixes counter/proof sync issues)

```text
Usage: restore --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

### Agent-First Pay cashu limit - Spend limit for cashu network or a specific cashu wallet

```text
Usage: limit [OPTIONS] <COMMAND>

Commands:
  add   Add a network or wallet spend limit
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID (omit for network-level limit)

  -h, --help
          Print help
```

#### Agent-First Pay cashu limit add - Add a network or wallet spend limit

```text
Usage: add --window <WINDOW> --max-spend <MAX_SPEND>

Options:
      --window <WINDOW>
          Time window: e.g. 30m, 1h, 24h, 7d

      --max-spend <MAX_SPEND>
          Maximum spend in base units

  -h, --help
          Print help
```

### Agent-First Pay cashu config - Per-wallet configuration

```text
Usage: config --wallet <WALLET> <COMMAND>

Commands:
  show  Show current wallet configuration
  set   Update wallet settings
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

#### Agent-First Pay cashu config show - Show current wallet configuration

```text
Usage: show

Options:
  -h, --help
          Print help
```

#### Agent-First Pay cashu config set - Update wallet settings

```text
Usage: set [OPTIONS]

Options:
      --label <LABEL>
          New label

  -h, --help
          Print help
```

### Agent-First Pay cashu backup - Back up cashu wallet data to a .tar.zst archive

```text
Usage: backup [OPTIONS]

Options:
      --output <OUTPUT>
          Output archive path (default: ./afpay-cashu-{timestamp}.tar.zst)

      --wallet <WALLET>
          Wallet ID (omit to back up all cashu wallets)

  -h, --help
          Print help
```

### Agent-First Pay cashu restore - Restore cashu wallet data from a .tar.zst archive

```text
Usage: restore [OPTIONS] <ARCHIVE>

Arguments:
  <ARCHIVE>
          Path to the backup archive

Options:
      --dangerously-overwrite
          Clear existing data before restoring (default: merge)

      --pg-url-secret <PG_URL_SECRET>
          Override PostgreSQL connection URL for the pg restore step

  -h, --help
          Print help
```

## Agent-First Pay ln - Lightning Network operations (NWC, phoenixd, LNbits)

```text
Usage: ln <COMMAND>

Commands:
  wallet   Wallet management
  send     Pay a Lightning invoice or BOLT12 offer
  receive  Create a Lightning invoice (BOLT11) or get a reusable BOLT12 offer
  balance  Check balance
  limit    Spend limit for ln network or a specific ln wallet
  config   Per-wallet configuration
  backup   Back up Lightning wallet data to a .tar.zst archive
  restore  Restore Lightning wallet data from a .tar.zst archive
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay ln wallet - Wallet management

```text
Usage: wallet <COMMAND>

Commands:
  create                 Create a new Lightning wallet
  close                  Close a Lightning wallet
  list                   List Lightning wallets
  dangerously-show-seed  Dangerously show wallet seed (for LN this is backend credential, not mnemonic words)
  help                   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay ln wallet create - Create a new Lightning wallet

```text
Usage: create [OPTIONS] --backend <BACKEND>

Options:
      --backend <BACKEND>
          Backend: nwc, phoenixd, lnbits

          [possible values: nwc, phoenixd, lnbits]

      --nwc-uri-secret <NWC_URI_SECRET>
          NWC connection URI secret (for nwc backend)

      --endpoint <ENDPOINT>
          Endpoint URL (for phoenixd, lnbits)

      --password-secret <PASSWORD_SECRET>
          Password secret (for phoenixd)

      --admin-key-secret <ADMIN_KEY_SECRET>
          Admin API key secret (for lnbits)

      --label <LABEL>
          Optional label

  -h, --help
          Print help
```

#### Agent-First Pay ln wallet close - Close a Lightning wallet

```text
Usage: close [OPTIONS] --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

      --dangerously-skip-balance-check-and-may-lose-money
          Dangerously skip balance checks when closing wallet

  -h, --help
          Print help
```

#### Agent-First Pay ln wallet list - List Lightning wallets

```text
Usage: list

Options:
  -h, --help
          Print help
```

#### Agent-First Pay ln wallet dangerously-show-seed - Dangerously show wallet seed (for LN this is backend credential, not mnemonic words)

```text
Usage: dangerously-show-seed --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

### Agent-First Pay ln send - Pay a Lightning invoice or BOLT12 offer

```text
Usage: send [OPTIONS] --to <TO>

Options:
      --to <TO>
          BOLT11 invoice or BOLT12 offer (lno1…) to pay

      --amount-sats <AMOUNT_SATS>
          Amount in sats (required for BOLT12 offers, rejected for BOLT11)

      --wallet <WALLET>
          Source wallet ID (auto-selected if omitted)

      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo (sent with the transaction)

      --local-memo <LOCAL_MEMO>
          Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)

      --idempotency-key <IDEMPOTENCY_KEY>
          Opaque idempotency key (≤128 chars). A second send with the same key and identical body replays the first response instead of re-broadcasting; a different body returns idempotency_conflict. Persisted for 24h

  -h, --help
          Print help
```

### Agent-First Pay ln receive - Create a Lightning invoice (BOLT11) or get a reusable BOLT12 offer

```text
Usage: receive [OPTIONS]

Options:
      --amount-sats <AMOUNT_SATS>
          Amount in sats (omit for BOLT12 offer)

      --wallet <WALLET>
          Wallet ID (auto-selected if omitted)

      --wait
          Wait for payment / matching receive transaction

      --wait-timeout-s <WAIT_TIMEOUT_S>
          Timeout in seconds for --wait

      --wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>
          Poll interval in milliseconds for --wait

      --qr-svg-file
          Write receive QR payload to an SVG file

  -h, --help
          Print help
```

### Agent-First Pay ln balance - Check balance

```text
Usage: balance [OPTIONS]

Options:
      --wallet <WALLET>
          Wallet ID (omit to show all ln wallets)

  -h, --help
          Print help
```

### Agent-First Pay ln limit - Spend limit for ln network or a specific ln wallet

```text
Usage: limit [OPTIONS] <COMMAND>

Commands:
  add   Add a network or wallet spend limit
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID (omit for network-level limit)

  -h, --help
          Print help
```

#### Agent-First Pay ln limit add - Add a network or wallet spend limit

```text
Usage: add --window <WINDOW> --max-spend <MAX_SPEND>

Options:
      --window <WINDOW>
          Time window: e.g. 30m, 1h, 24h, 7d

      --max-spend <MAX_SPEND>
          Maximum spend in base units

  -h, --help
          Print help
```

### Agent-First Pay ln config - Per-wallet configuration

```text
Usage: config --wallet <WALLET> <COMMAND>

Commands:
  show  Show current wallet configuration
  set   Update wallet settings
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

#### Agent-First Pay ln config show - Show current wallet configuration

```text
Usage: show

Options:
  -h, --help
          Print help
```

#### Agent-First Pay ln config set - Update wallet settings

```text
Usage: set [OPTIONS]

Options:
      --label <LABEL>
          New label

  -h, --help
          Print help
```

### Agent-First Pay ln backup - Back up Lightning wallet data to a .tar.zst archive

```text
Usage: backup [OPTIONS]

Options:
      --output <OUTPUT>
          Output archive path (default: ./afpay-ln-{timestamp}.tar.zst)

      --wallet <WALLET>
          Wallet ID (omit to back up all ln wallets)

  -h, --help
          Print help
```

### Agent-First Pay ln restore - Restore Lightning wallet data from a .tar.zst archive

```text
Usage: restore [OPTIONS] <ARCHIVE>

Arguments:
  <ARCHIVE>
          Path to the backup archive

Options:
      --dangerously-overwrite
          Clear existing data before restoring (default: merge)

      --pg-url-secret <PG_URL_SECRET>
          Override PostgreSQL connection URL for the pg restore step

  -h, --help
          Print help
```

## Agent-First Pay sol - Solana operations

```text
Usage: sol <COMMAND>

Commands:
  wallet   Wallet management
  send     Send SOL or SPL token transfer
  receive  Show wallet receive address
  balance  Check balance
  limit    Spend limit for sol network or a specific sol wallet
  config   Per-wallet configuration
  backup   Back up Solana wallet data to a .tar.zst archive
  restore  Restore Solana wallet data from a .tar.zst archive
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay sol wallet - Wallet management

```text
Usage: wallet <COMMAND>

Commands:
  create                 Create a new Solana wallet
  close                  Close a Solana wallet
  list                   List Solana wallets
  dangerously-show-seed  Dangerously show wallet seed mnemonic (12 BIP39 words)
  help                   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay sol wallet create - Create a new Solana wallet

```text
Usage: create [OPTIONS] --sol-rpc-endpoint <SOL_RPC_ENDPOINT>

Options:
      --sol-rpc-endpoint <SOL_RPC_ENDPOINT>
          Solana JSON-RPC endpoint (repeat to configure failover order)

      --label <LABEL>
          Optional label

      --sol-cluster <SOL_CLUSTER>
          Solana cluster tag. Stored on the wallet; sends to a different cluster are rejected. Accepted: mainnet-beta, devnet, testnet

  -h, --help
          Print help
```

#### Agent-First Pay sol wallet close - Close a Solana wallet

```text
Usage: close [OPTIONS] --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

      --dangerously-skip-balance-check-and-may-lose-money
          Dangerously skip balance checks when closing wallet

  -h, --help
          Print help
```

#### Agent-First Pay sol wallet list - List Solana wallets

```text
Usage: list

Options:
  -h, --help
          Print help
```

#### Agent-First Pay sol wallet dangerously-show-seed - Dangerously show wallet seed mnemonic (12 BIP39 words)

```text
Usage: dangerously-show-seed --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

### Agent-First Pay sol send - Send SOL or SPL token transfer

```text
Usage: send [OPTIONS] --to <TO> --amount <AMOUNT> --token <TOKEN>

Options:
      --to <TO>
          Recipient Solana address (base58)

      --amount <AMOUNT>
          Amount in token base units (lamports for SOL, smallest unit for SPL tokens)

      --token <TOKEN>
          Token: "native" for SOL, "usdc", "usdt", or SPL mint address

      --reference <REFERENCE>
          Reference key for order binding (base58-encoded 32 bytes, per strain-payment-method-solana)

      --wallet <WALLET>
          Source wallet ID (auto-selected if omitted)

      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo (sent with the transaction)

      --local-memo <LOCAL_MEMO>
          Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)

      --idempotency-key <IDEMPOTENCY_KEY>
          Opaque idempotency key (≤128 chars). A second send with the same key and identical body replays the first response instead of re-broadcasting; a different body returns idempotency_conflict. Persisted for 24h

  -h, --help
          Print help
```

### Agent-First Pay sol receive - Show wallet receive address

```text
Usage: receive [OPTIONS]

Options:
      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo to watch for (used with --wait)

      --min-confirmations <MIN_CONFIRMATIONS>
          Minimum confirmation depth before considering payment settled (requires --wait)

      --reference <REFERENCE>
          Reference key to watch for (base58, used with --wait, per strain-payment-method-solana)

      --wallet <WALLET>
          Wallet ID (auto-selected if omitted)

      --wait
          Wait for payment / matching receive transaction

      --wait-timeout-s <WAIT_TIMEOUT_S>
          Timeout in seconds for --wait

      --wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>
          Poll interval in milliseconds for --wait

      --qr-svg-file
          Write receive QR payload to an SVG file

  -h, --help
          Print help
```

### Agent-First Pay sol balance - Check balance

```text
Usage: balance [OPTIONS]

Options:
      --wallet <WALLET>
          Wallet ID (omit to show all sol wallets)

  -h, --help
          Print help
```

### Agent-First Pay sol limit - Spend limit for sol network or a specific sol wallet

```text
Usage: limit [OPTIONS] <COMMAND>

Commands:
  add   Add a network or wallet spend limit
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID (omit for network-level limit)

  -h, --help
          Print help
```

#### Agent-First Pay sol limit add - Add a network or wallet spend limit

```text
Usage: add [OPTIONS] --window <WINDOW> --max-spend <MAX_SPEND>

Options:
      --token <TOKEN>
          Token: native, usdc, usdt

      --window <WINDOW>
          Time window: e.g. 30m, 1h, 24h, 7d

      --max-spend <MAX_SPEND>
          Maximum spend in base units

  -h, --help
          Print help
```

### Agent-First Pay sol config - Per-wallet configuration

```text
Usage: config --wallet <WALLET> <COMMAND>

Commands:
  show          Show current wallet configuration
  set           Update wallet settings
  token-add     Register a custom token for balance tracking
  token-remove  Unregister a custom token
  help          Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

#### Agent-First Pay sol config show - Show current wallet configuration

```text
Usage: show

Options:
  -h, --help
          Print help
```

#### Agent-First Pay sol config set - Update wallet settings

```text
Usage: set [OPTIONS]

Options:
      --label <LABEL>
          New label

      --rpc-endpoint <RPC_ENDPOINT>
          Replace RPC endpoint(s)

  -h, --help
          Print help
```

#### Agent-First Pay sol config token-add - Register a custom token for balance tracking

```text
Usage: token-add [OPTIONS] --symbol <SYMBOL> --address <ADDRESS>

Options:
      --symbol <SYMBOL>
          Token symbol (e.g. dai)

      --address <ADDRESS>
          Token contract address

      --decimals <DECIMALS>
          Token decimals

          [default: 6]

  -h, --help
          Print help
```

#### Agent-First Pay sol config token-remove - Unregister a custom token

```text
Usage: token-remove --symbol <SYMBOL>

Options:
      --symbol <SYMBOL>
          Token symbol to remove

  -h, --help
          Print help
```

### Agent-First Pay sol backup - Back up Solana wallet data to a .tar.zst archive

```text
Usage: backup [OPTIONS]

Options:
      --output <OUTPUT>
          Output archive path (default: ./afpay-sol-{timestamp}.tar.zst)

      --wallet <WALLET>
          Wallet ID (omit to back up all sol wallets)

  -h, --help
          Print help
```

### Agent-First Pay sol restore - Restore Solana wallet data from a .tar.zst archive

```text
Usage: restore [OPTIONS] <ARCHIVE>

Arguments:
  <ARCHIVE>
          Path to the backup archive

Options:
      --dangerously-overwrite
          Clear existing data before restoring (default: merge)

      --pg-url-secret <PG_URL_SECRET>
          Override PostgreSQL connection URL for the pg restore step

  -h, --help
          Print help
```

## Agent-First Pay evm - EVM chain operations (Base, Arbitrum)

```text
Usage: evm <COMMAND>

Commands:
  wallet   Wallet management
  send     Send native token or ERC-20 token transfer
  receive  Show wallet receive address
  balance  Check balance
  limit    Spend limit for evm network or a specific evm wallet
  config   Per-wallet configuration
  backup   Back up EVM wallet data to a .tar.zst archive
  restore  Restore EVM wallet data from a .tar.zst archive
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay evm wallet - Wallet management

```text
Usage: wallet <COMMAND>

Commands:
  create                 Create a new EVM chain wallet
  close                  Close an EVM chain wallet
  list                   List EVM chain wallets
  dangerously-show-seed  Dangerously show wallet seed mnemonic (12 BIP39 words)
  help                   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay evm wallet create - Create a new EVM chain wallet

```text
Usage: create [OPTIONS] --evm-rpc-endpoint <EVM_RPC_ENDPOINT>

Options:
      --evm-rpc-endpoint <EVM_RPC_ENDPOINT>
          EVM JSON-RPC endpoint (repeat to configure failover order)

      --chain-id <CHAIN_ID>
          Chain ID (default: 8453 = Base)

          [default: 8453]

      --label <LABEL>
          Optional label

  -h, --help
          Print help
```

#### Agent-First Pay evm wallet close - Close an EVM chain wallet

```text
Usage: close [OPTIONS] --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

      --dangerously-skip-balance-check-and-may-lose-money
          Dangerously skip balance checks when closing wallet

  -h, --help
          Print help
```

#### Agent-First Pay evm wallet list - List EVM chain wallets

```text
Usage: list

Options:
  -h, --help
          Print help
```

#### Agent-First Pay evm wallet dangerously-show-seed - Dangerously show wallet seed mnemonic (12 BIP39 words)

```text
Usage: dangerously-show-seed --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

### Agent-First Pay evm send - Send native token or ERC-20 token transfer

```text
Usage: send [OPTIONS] --to <TO> --amount <AMOUNT> --token <TOKEN>

Options:
      --to <TO>
          Recipient address (0x...)

      --amount <AMOUNT>
          Amount in token base units (wei for ETH, smallest unit for ERC-20)

      --token <TOKEN>
          Token: "native" for chain native, "usdc" or contract address for ERC-20

      --chain-id <CHAIN_ID>
          Optional chain_id pin. When set, the daemon verifies the wallet's chain_id matches before broadcasting. Mismatch returns wrong_chain

      --wallet <WALLET>
          Source wallet ID (auto-selected if omitted)

      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo (sent with the transaction)

      --local-memo <LOCAL_MEMO>
          Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)

      --idempotency-key <IDEMPOTENCY_KEY>
          Opaque idempotency key (≤128 chars). A second send with the same key and identical body replays the first response instead of re-broadcasting; a different body returns idempotency_conflict. Persisted for 24h

  -h, --help
          Print help
```

### Agent-First Pay evm receive - Show wallet receive address

```text
Usage: receive [OPTIONS]

Options:
      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo to watch for (used with --wait)

      --min-confirmations <MIN_CONFIRMATIONS>
          Minimum confirmation depth before considering payment settled (requires --wait)

      --wallet <WALLET>
          Wallet ID (auto-selected if omitted)

      --wait
          Wait for payment / matching receive transaction

      --wait-timeout-s <WAIT_TIMEOUT_S>
          Timeout in seconds for --wait

      --wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>
          Poll interval in milliseconds for --wait

      --qr-svg-file
          Write receive QR payload to an SVG file

  -h, --help
          Print help
```

### Agent-First Pay evm balance - Check balance

```text
Usage: balance [OPTIONS]

Options:
      --wallet <WALLET>
          Wallet ID (omit to show all evm wallets)

  -h, --help
          Print help
```

### Agent-First Pay evm limit - Spend limit for evm network or a specific evm wallet

```text
Usage: limit [OPTIONS] <COMMAND>

Commands:
  add   Add a network or wallet spend limit
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID (omit for network-level limit)

  -h, --help
          Print help
```

#### Agent-First Pay evm limit add - Add a network or wallet spend limit

```text
Usage: add [OPTIONS] --window <WINDOW> --max-spend <MAX_SPEND>

Options:
      --token <TOKEN>
          Token: native, usdc, usdt

      --window <WINDOW>
          Time window: e.g. 30m, 1h, 24h, 7d

      --max-spend <MAX_SPEND>
          Maximum spend in base units

  -h, --help
          Print help
```

### Agent-First Pay evm config - Per-wallet configuration

```text
Usage: config --wallet <WALLET> <COMMAND>

Commands:
  show          Show current wallet configuration
  set           Update wallet settings
  token-add     Register a custom token for balance tracking
  token-remove  Unregister a custom token
  help          Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

#### Agent-First Pay evm config show - Show current wallet configuration

```text
Usage: show

Options:
  -h, --help
          Print help
```

#### Agent-First Pay evm config set - Update wallet settings

```text
Usage: set [OPTIONS]

Options:
      --label <LABEL>
          New label

      --rpc-endpoint <RPC_ENDPOINT>
          Replace RPC endpoint(s)

      --chain-id <CHAIN_ID>
          EVM chain ID

  -h, --help
          Print help
```

#### Agent-First Pay evm config token-add - Register a custom token for balance tracking

```text
Usage: token-add [OPTIONS] --symbol <SYMBOL> --address <ADDRESS>

Options:
      --symbol <SYMBOL>
          Token symbol (e.g. dai)

      --address <ADDRESS>
          Token contract address

      --decimals <DECIMALS>
          Token decimals

          [default: 6]

  -h, --help
          Print help
```

#### Agent-First Pay evm config token-remove - Unregister a custom token

```text
Usage: token-remove --symbol <SYMBOL>

Options:
      --symbol <SYMBOL>
          Token symbol to remove

  -h, --help
          Print help
```

### Agent-First Pay evm backup - Back up EVM wallet data to a .tar.zst archive

```text
Usage: backup [OPTIONS]

Options:
      --output <OUTPUT>
          Output archive path (default: ./afpay-evm-{timestamp}.tar.zst)

      --wallet <WALLET>
          Wallet ID (omit to back up all evm wallets)

  -h, --help
          Print help
```

### Agent-First Pay evm restore - Restore EVM wallet data from a .tar.zst archive

```text
Usage: restore [OPTIONS] <ARCHIVE>

Arguments:
  <ARCHIVE>
          Path to the backup archive

Options:
      --dangerously-overwrite
          Clear existing data before restoring (default: merge)

      --pg-url-secret <PG_URL_SECRET>
          Override PostgreSQL connection URL for the pg restore step

  -h, --help
          Print help
```

## Agent-First Pay btc - Bitcoin on-chain operations

```text
Usage: btc <COMMAND>

Commands:
  wallet   Wallet management
  send     Send BTC on-chain
  receive  Show wallet receive address
  balance  Check balance
  limit    Spend limit for btc network or a specific btc wallet
  config   Per-wallet configuration
  backup   Back up Bitcoin wallet data to a .tar.zst archive
  restore  Restore Bitcoin wallet data from a .tar.zst archive
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay btc wallet - Wallet management

```text
Usage: wallet <COMMAND>

Commands:
  create                 Create a new Bitcoin wallet
  close                  Close a Bitcoin wallet
  list                   List Bitcoin wallets
  dangerously-show-seed  Dangerously show wallet seed mnemonic (12 BIP39 words)
  help                   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

#### Agent-First Pay btc wallet create - Create a new Bitcoin wallet

```text
Usage: create [OPTIONS]

Options:
      --btc-network <BTC_NETWORK>
          Bitcoin sub-network: mainnet or signet (default: mainnet)

          [default: mainnet]

      --btc-address-type <BTC_ADDRESS_TYPE>
          Address type: taproot or segwit (default: taproot)

          [default: taproot]

      --btc-backend <BTC_BACKEND>
          Chain-source backend: esplora (default), core-rpc, electrum

          [possible values: esplora, core-rpc, electrum]

      --btc-esplora-url <BTC_ESPLORA_URL>
          Custom Esplora API URL

      --btc-core-url <BTC_CORE_URL>
          Bitcoin Core RPC URL (core-rpc backend)

      --btc-core-auth-secret <BTC_CORE_AUTH_SECRET>
          Bitcoin Core RPC auth "user:pass" (core-rpc backend)

      --btc-electrum-url <BTC_ELECTRUM_URL>
          Electrum server URL (electrum backend)

      --mnemonic-secret <MNEMONIC_SECRET>
          Existing BIP39 mnemonic secret to restore wallet

      --label <LABEL>
          Optional label

  -h, --help
          Print help
```

#### Agent-First Pay btc wallet close - Close a Bitcoin wallet

```text
Usage: close [OPTIONS] --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

      --dangerously-skip-balance-check-and-may-lose-money
          Dangerously skip balance checks when closing wallet

  -h, --help
          Print help
```

#### Agent-First Pay btc wallet list - List Bitcoin wallets

```text
Usage: list

Options:
  -h, --help
          Print help
```

#### Agent-First Pay btc wallet dangerously-show-seed - Dangerously show wallet seed mnemonic (12 BIP39 words)

```text
Usage: dangerously-show-seed --wallet <WALLET>

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

### Agent-First Pay btc send - Send BTC on-chain

```text
Usage: send [OPTIONS] --to <TO> --amount-sats <AMOUNT_SATS>

Options:
      --to <TO>
          Recipient Bitcoin address (bc1.../tb1...)

      --amount-sats <AMOUNT_SATS>
          Amount in satoshis

      --wallet <WALLET>
          Source wallet ID (auto-selected if omitted)

      --onchain-memo <ONCHAIN_MEMO>
          On-chain memo (sent with the transaction)

      --local-memo <LOCAL_MEMO>
          Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)

      --idempotency-key <IDEMPOTENCY_KEY>
          Opaque idempotency key (≤128 chars). A second send with the same key and identical body replays the first response instead of re-broadcasting; a different body returns idempotency_conflict. Persisted for 24h

  -h, --help
          Print help
```

### Agent-First Pay btc receive - Show wallet receive address

```text
Usage: receive [OPTIONS]

Options:
      --wait-sync-limit <WAIT_SYNC_LIMIT>
          Max history records scanned per poll when resolving tx id

      --wallet <WALLET>
          Wallet ID (auto-selected if omitted)

      --wait
          Wait for payment / matching receive transaction

      --wait-timeout-s <WAIT_TIMEOUT_S>
          Timeout in seconds for --wait

      --wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>
          Poll interval in milliseconds for --wait

      --qr-svg-file
          Write receive QR payload to an SVG file

  -h, --help
          Print help
```

### Agent-First Pay btc balance - Check balance

```text
Usage: balance [OPTIONS]

Options:
      --wallet <WALLET>
          Wallet ID (omit to show all btc wallets)

  -h, --help
          Print help
```

### Agent-First Pay btc limit - Spend limit for btc network or a specific btc wallet

```text
Usage: limit [OPTIONS] <COMMAND>

Commands:
  add   Add a network or wallet spend limit
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID (omit for network-level limit)

  -h, --help
          Print help
```

#### Agent-First Pay btc limit add - Add a network or wallet spend limit

```text
Usage: add --window <WINDOW> --max-spend <MAX_SPEND>

Options:
      --window <WINDOW>
          Time window: e.g. 30m, 1h, 24h, 7d

      --max-spend <MAX_SPEND>
          Maximum spend in base units

  -h, --help
          Print help
```

### Agent-First Pay btc config - Per-wallet configuration

```text
Usage: config --wallet <WALLET> <COMMAND>

Commands:
  show  Show current wallet configuration
  set   Update wallet settings
  help  Print this message or the help of the given subcommand(s)

Options:
      --wallet <WALLET>
          Wallet ID

  -h, --help
          Print help
```

#### Agent-First Pay btc config show - Show current wallet configuration

```text
Usage: show

Options:
  -h, --help
          Print help
```

#### Agent-First Pay btc config set - Update wallet settings

```text
Usage: set [OPTIONS]

Options:
      --label <LABEL>
          New label

  -h, --help
          Print help
```

### Agent-First Pay btc backup - Back up Bitcoin wallet data to a .tar.zst archive

```text
Usage: backup [OPTIONS]

Options:
      --output <OUTPUT>
          Output archive path (default: ./afpay-btc-{timestamp}.tar.zst)

      --wallet <WALLET>
          Wallet ID (omit to back up all btc wallets)

  -h, --help
          Print help
```

### Agent-First Pay btc restore - Restore Bitcoin wallet data from a .tar.zst archive

```text
Usage: restore [OPTIONS] <ARCHIVE>

Arguments:
  <ARCHIVE>
          Path to the backup archive

Options:
      --dangerously-overwrite
          Clear existing data before restoring (default: merge)

      --pg-url-secret <PG_URL_SECRET>
          Override PostgreSQL connection URL for the pg restore step

  -h, --help
          Print help
```

## Agent-First Pay wallet - List all wallets (cross-network)

```text
Usage: wallet <COMMAND>

Commands:
  list  List all wallets (cross-network)
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay wallet list - List all wallets (cross-network)

```text
Usage: list [OPTIONS]

Options:
      --network <NETWORK>
          Filter by network: cashu, ln, sol, evm

          [possible values: ln, sol, evm, cashu, btc]

  -h, --help
          Print help
```

## Agent-First Pay balance - All wallets balance (cross-network)

```text
Usage: balance [OPTIONS]

Options:
      --wallet <WALLET>
          Wallet ID (omit to show all wallets)

      --network <NETWORK>
          Filter by network: cashu, ln, sol, evm

          [possible values: ln, sol, evm, cashu, btc]

      --cashu-check
          Verify cashu proofs against mint (slower but accurate; cashu only)

  -h, --help
          Print help
```

## Agent-First Pay history - History queries

```text
Usage: history <COMMAND>

Commands:
  list    List history records from local store
  status  Check history status
  update  Incrementally sync on-chain/backend history into local store
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay history list - List history records from local store

```text
Usage: list [OPTIONS]

Options:
      --wallet <WALLET>
          Filter by wallet ID

      --network <NETWORK>
          Filter by network: cashu, ln, sol, evm

          [possible values: ln, sol, evm, cashu, btc]

      --onchain-memo <ONCHAIN_MEMO>
          Filter by exact on-chain memo text

      --limit <LIMIT>
          Max results

          [default: 20]

      --offset <OFFSET>
          Offset

          [default: 0]

      --since-epoch-s <SINCE_EPOCH_S>
          Only include records created at or after this epoch second

      --until-epoch-s <UNTIL_EPOCH_S>
          Only include records created before this epoch second

  -h, --help
          Print help
```

### Agent-First Pay history status - Check history status

```text
Usage: status --transaction-id <TRANSACTION_ID>

Options:
      --transaction-id <TRANSACTION_ID>
          Transaction ID

  -h, --help
          Print help
```

### Agent-First Pay history update - Incrementally sync on-chain/backend history into local store

```text
Usage: update [OPTIONS]

Options:
      --wallet <WALLET>
          Sync a specific wallet (defaults to all wallets in scope)

      --network <NETWORK>
          Restrict sync to a single network

          [possible values: ln, sol, evm, cashu, btc]

      --limit <LIMIT>
          Max records to scan per wallet during this incremental sync

          [default: 200]

  -h, --help
          Print help
```

## Agent-First Pay limit - Spend limit list and remove (cross-network)

```text
Usage: limit <COMMAND>

Commands:
  remove     Remove a spend limit rule by ID
  list       List current limit status
  reconcile  Manually reconcile a stuck spend-ledger reservation (operator-only)
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay limit remove - Remove a spend limit rule by ID

```text
Usage: remove --rule-id <RULE_ID>

Options:
      --rule-id <RULE_ID>
          Rule ID (e.g. r_1a2b3c4d)

  -h, --help
          Print help
```

### Agent-First Pay limit list - List current limit status

```text
Usage: list

Options:
  -h, --help
          Print help
```

### Agent-First Pay limit reconcile - Manually reconcile a stuck spend-ledger reservation (operator-only)

Manually reconcile a stuck spend-ledger reservation (operator-only).

Use when AccountingInconsistent fired (money sent but ledger could not confirm) or when a BTC settlement crossed the reservation TTL. Pass `--confirm` if the payment actually succeeded (writes a spend event so the limit reflects the spend), or `--cancel` if it did not.

```text
Usage: reconcile [OPTIONS] --reservation-id <RESERVATION_ID> --reason <REASON>

Options:
      --reservation-id <RESERVATION_ID>
          Reservation ID (numeric, from limit_list / Output::Sent.reservation_ids)

      --confirm
          Mark the reservation Confirmed (mutually exclusive with --cancel)

      --cancel
          Mark the reservation Cancelled (mutually exclusive with --confirm)

      --reason <REASON>
          Required audit note (1..=512 chars) — why this reservation is being forced to a terminal state

  -h, --help
          Print help
```

## Agent-First Pay skill - Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode, Hermes)

```text
Usage: skill <COMMAND>

Commands:
  status     Show whether the skill is installed, valid, and up to date
  install    Install or refresh the skill
  uninstall  Remove a managed skill
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay skill status - Show whether the skill is installed, valid, and up to date

```text
Usage: status [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage. Defaults to every agent that supports the requested scope

          Possible values:
          - all:         Manage every agent that supports the requested scope
          - codex:       Manage the Codex local skill under $CODEX_HOME/skills
          - claude-code: Manage the Claude Code skill under ~/.claude/skills or .claude/skills
          - opencode:    Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills
          - hermes:      Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills

          [default: all]

      --scope <SCOPE>
          Skill scope

          Possible values:
          - personal:  Install under the user-level skills directory
          - workspace: Install under the current workspace's skills directory

          [default: personal]

      --skills-dir <SKILLS_DIR>
          Directory that contains skill folders. Requires an explicit single --agent

  -h, --help
          Print help (see a summary with '-h')
```

### Agent-First Pay skill install - Install or refresh the skill

```text
Usage: install [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage. Defaults to every agent that supports the requested scope

          Possible values:
          - all:         Manage every agent that supports the requested scope
          - codex:       Manage the Codex local skill under $CODEX_HOME/skills
          - claude-code: Manage the Claude Code skill under ~/.claude/skills or .claude/skills
          - opencode:    Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills
          - hermes:      Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills

          [default: all]

      --scope <SCOPE>
          Skill scope

          Possible values:
          - personal:  Install under the user-level skills directory
          - workspace: Install under the current workspace's skills directory

          [default: personal]

      --skills-dir <SKILLS_DIR>
          Directory that contains skill folders. Requires an explicit single --agent

      --force
          Overwrite or remove a skill this tool did not manage

  -h, --help
          Print help (see a summary with '-h')
```

### Agent-First Pay skill uninstall - Remove a managed skill

```text
Usage: uninstall [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage. Defaults to every agent that supports the requested scope

          Possible values:
          - all:         Manage every agent that supports the requested scope
          - codex:       Manage the Codex local skill under $CODEX_HOME/skills
          - claude-code: Manage the Claude Code skill under ~/.claude/skills or .claude/skills
          - opencode:    Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills
          - hermes:      Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills

          [default: all]

      --scope <SCOPE>
          Skill scope

          Possible values:
          - personal:  Install under the user-level skills directory
          - workspace: Install under the current workspace's skills directory

          [default: personal]

      --skills-dir <SKILLS_DIR>
          Directory that contains skill folders. Requires an explicit single --agent

      --force
          Overwrite or remove a skill this tool did not manage

  -h, --help
          Print help (see a summary with '-h')
```

## Agent-First Pay container - Build and run the afpay daemon container (Docker, Podman, or Apple) from the embedded recipe

```text
Usage: container <COMMAND>

Commands:
  install    Build the image if missing and run the daemon; print the client command
  uninstall  Stop and remove the container (--purge also removes the image and cache)
  status     Report whether the daemon is running, with its endpoint and client command
  logs       Stream the container logs (raw passthrough)
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### Agent-First Pay container install - Build the image if missing and run the daemon; print the client command

```text
Usage: install [OPTIONS]

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          [possible values: docker, podman, apple]

      --name <NAME>
          Container name

          [default: afpay]

      --port <PORT>
          Daemon port, published on 127.0.0.1

          [default: 9401]

      --mode <MODE>
          Server mode: rest (HTTP + bearer key) or rpc (gRPC + PSK)

          [default: rest]
          [possible values: rest, rpc]

      --with <DAEMON>
          Optional bundled daemon to install and enable (repeatable): phoenixd, bitcoind

      --allow <CATEGORY=URL>
          Operator allowlist entry (repeatable), `<category>=<url>`. afpay refuses to expose a public listener without one. Categories: mint, esplora, sol-rpc, evm-rpc, btc-core, btc-electrum, ln

      --btc-network <BTC_NETWORK>
          Bitcoin network when --with bitcoind: mainnet or signet

          [default: mainnet]

      --btc-rpc-port <BTC_RPC_PORT>
          bitcoind RPC port when --with bitcoind

          [default: 8332]

      --btc-prune-mb <BTC_PRUNE_MB>
          bitcoind prune target (MB) when --with bitcoind

          [default: 550]

      --features <FEATURES>
          Cargo feature set for --from-source builds (defaults to the Dockerfile's set)

      --rebuild
          Rebuild the image even if it already exists

      --from-source
          Build the full image from a source checkout instead of downloading the prebuilt release

      --context <DIR>
          Source checkout to build from with --from-source (default: current dir)

  -h, --help
          Print help
```

### Agent-First Pay container uninstall - Stop and remove the container (--purge also removes the image and cache)

```text
Usage: uninstall [OPTIONS]

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          [possible values: docker, podman, apple]

      --name <NAME>
          Container name

          [default: afpay]

      --purge
          Also remove the built image and the cached build context

  -h, --help
          Print help
```

### Agent-First Pay container status - Report whether the daemon is running, with its endpoint and client command

```text
Usage: status [OPTIONS]

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          [possible values: docker, podman, apple]

      --name <NAME>
          Container name

          [default: afpay]

      --port <PORT>
          Published port, used to format the endpoint and client command

          [default: 9401]

      --mode <MODE>
          Server mode, used to pick the secret file (rest-api-key vs rpc-secret)

          [default: rest]
          [possible values: rest, rpc]

  -h, --help
          Print help
```

### Agent-First Pay container logs - Stream the container logs (raw passthrough)

```text
Usage: logs [OPTIONS]

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          [possible values: docker, podman, apple]

      --name <NAME>
          Container name

          [default: afpay]

  -f, --follow
          Follow the log output

  -h, --help
          Print help
```
AFDATA: 0.22.0
