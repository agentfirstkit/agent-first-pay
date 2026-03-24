<!-- Generated from src/cli.rs. Do not edit by hand. -->

# afpay CLI Reference

> Generated from `src/cli.rs`.
> Regenerate with `./scripts/generate-cli-doc.sh`.
> See [../README.md](../README.md) for setup and examples, and [architecture.md](architecture.md) for deployment details.

This document contains the help content for the `afpay` command-line program.

**Command Overview:**

* [`afpay`↴](#afpay)
* [`afpay global`↴](#afpay-global)
* [`afpay global limit`↴](#afpay-global-limit)
* [`afpay global limit add`↴](#afpay-global-limit-add)
* [`afpay global config`↴](#afpay-global-config)
* [`afpay global config show`↴](#afpay-global-config-show)
* [`afpay global config set`↴](#afpay-global-config-set)
* [`afpay cashu`↴](#afpay-cashu)
* [`afpay cashu send`↴](#afpay-cashu-send)
* [`afpay cashu receive`↴](#afpay-cashu-receive)
* [`afpay cashu send-to-ln`↴](#afpay-cashu-send-to-ln)
* [`afpay cashu receive-from-ln`↴](#afpay-cashu-receive-from-ln)
* [`afpay cashu receive-from-ln-claim`↴](#afpay-cashu-receive-from-ln-claim)
* [`afpay cashu balance`↴](#afpay-cashu-balance)
* [`afpay cashu wallet`↴](#afpay-cashu-wallet)
* [`afpay cashu wallet create`↴](#afpay-cashu-wallet-create)
* [`afpay cashu wallet close`↴](#afpay-cashu-wallet-close)
* [`afpay cashu wallet list`↴](#afpay-cashu-wallet-list)
* [`afpay cashu wallet dangerously-show-seed`↴](#afpay-cashu-wallet-dangerously-show-seed)
* [`afpay cashu wallet restore`↴](#afpay-cashu-wallet-restore)
* [`afpay cashu limit`↴](#afpay-cashu-limit)
* [`afpay cashu limit add`↴](#afpay-cashu-limit-add)
* [`afpay cashu config`↴](#afpay-cashu-config)
* [`afpay cashu config show`↴](#afpay-cashu-config-show)
* [`afpay cashu config set`↴](#afpay-cashu-config-set)
* [`afpay ln`↴](#afpay-ln)
* [`afpay ln wallet`↴](#afpay-ln-wallet)
* [`afpay ln wallet create`↴](#afpay-ln-wallet-create)
* [`afpay ln wallet close`↴](#afpay-ln-wallet-close)
* [`afpay ln wallet list`↴](#afpay-ln-wallet-list)
* [`afpay ln wallet dangerously-show-seed`↴](#afpay-ln-wallet-dangerously-show-seed)
* [`afpay ln send`↴](#afpay-ln-send)
* [`afpay ln receive`↴](#afpay-ln-receive)
* [`afpay ln balance`↴](#afpay-ln-balance)
* [`afpay ln limit`↴](#afpay-ln-limit)
* [`afpay ln limit add`↴](#afpay-ln-limit-add)
* [`afpay ln config`↴](#afpay-ln-config)
* [`afpay ln config show`↴](#afpay-ln-config-show)
* [`afpay ln config set`↴](#afpay-ln-config-set)
* [`afpay sol`↴](#afpay-sol)
* [`afpay sol wallet`↴](#afpay-sol-wallet)
* [`afpay sol wallet create`↴](#afpay-sol-wallet-create)
* [`afpay sol wallet close`↴](#afpay-sol-wallet-close)
* [`afpay sol wallet list`↴](#afpay-sol-wallet-list)
* [`afpay sol wallet dangerously-show-seed`↴](#afpay-sol-wallet-dangerously-show-seed)
* [`afpay sol send`↴](#afpay-sol-send)
* [`afpay sol receive`↴](#afpay-sol-receive)
* [`afpay sol balance`↴](#afpay-sol-balance)
* [`afpay sol limit`↴](#afpay-sol-limit)
* [`afpay sol limit add`↴](#afpay-sol-limit-add)
* [`afpay sol config`↴](#afpay-sol-config)
* [`afpay sol config show`↴](#afpay-sol-config-show)
* [`afpay sol config set`↴](#afpay-sol-config-set)
* [`afpay sol config token-add`↴](#afpay-sol-config-token-add)
* [`afpay sol config token-remove`↴](#afpay-sol-config-token-remove)
* [`afpay evm`↴](#afpay-evm)
* [`afpay evm wallet`↴](#afpay-evm-wallet)
* [`afpay evm wallet create`↴](#afpay-evm-wallet-create)
* [`afpay evm wallet close`↴](#afpay-evm-wallet-close)
* [`afpay evm wallet list`↴](#afpay-evm-wallet-list)
* [`afpay evm wallet dangerously-show-seed`↴](#afpay-evm-wallet-dangerously-show-seed)
* [`afpay evm send`↴](#afpay-evm-send)
* [`afpay evm receive`↴](#afpay-evm-receive)
* [`afpay evm balance`↴](#afpay-evm-balance)
* [`afpay evm limit`↴](#afpay-evm-limit)
* [`afpay evm limit add`↴](#afpay-evm-limit-add)
* [`afpay evm config`↴](#afpay-evm-config)
* [`afpay evm config show`↴](#afpay-evm-config-show)
* [`afpay evm config set`↴](#afpay-evm-config-set)
* [`afpay evm config token-add`↴](#afpay-evm-config-token-add)
* [`afpay evm config token-remove`↴](#afpay-evm-config-token-remove)
* [`afpay btc`↴](#afpay-btc)
* [`afpay btc wallet`↴](#afpay-btc-wallet)
* [`afpay btc wallet create`↴](#afpay-btc-wallet-create)
* [`afpay btc wallet close`↴](#afpay-btc-wallet-close)
* [`afpay btc wallet list`↴](#afpay-btc-wallet-list)
* [`afpay btc wallet dangerously-show-seed`↴](#afpay-btc-wallet-dangerously-show-seed)
* [`afpay btc send`↴](#afpay-btc-send)
* [`afpay btc receive`↴](#afpay-btc-receive)
* [`afpay btc balance`↴](#afpay-btc-balance)
* [`afpay btc limit`↴](#afpay-btc-limit)
* [`afpay btc limit add`↴](#afpay-btc-limit-add)
* [`afpay btc config`↴](#afpay-btc-config)
* [`afpay btc config show`↴](#afpay-btc-config-show)
* [`afpay btc config set`↴](#afpay-btc-config-set)
* [`afpay wallet`↴](#afpay-wallet)
* [`afpay wallet list`↴](#afpay-wallet-list)
* [`afpay balance`↴](#afpay-balance)
* [`afpay history`↴](#afpay-history)
* [`afpay history list`↴](#afpay-history-list)
* [`afpay history status`↴](#afpay-history-status)
* [`afpay history update`↴](#afpay-history-update)
* [`afpay limit`↴](#afpay-limit)
* [`afpay limit remove`↴](#afpay-limit-remove)
* [`afpay limit list`↴](#afpay-limit-list)

## `afpay`

Agent-first cryptocurrency micropayment tool

**Usage:** `afpay [OPTIONS] [COMMAND]`

###### **Subcommands:**

* `global` — Global (cross-network) operations
* `cashu` — Cashu operations
* `ln` — Lightning Network operations (NWC, phoenixd, LNbits)
* `sol` — Solana operations
* `evm` — EVM chain operations (Base, Arbitrum)
* `btc` — Bitcoin on-chain operations
* `wallet` — List all wallets (cross-network)
* `balance` — All wallets balance (cross-network)
* `history` — History queries
* `limit` — Spend limit list and remove (cross-network)

###### **Options:**

* `--mode <MODE>` — Run mode

  Default value: `cli`

  Possible values: `cli`, `pipe`, `interactive`, `tui`, `rpc`

* `--rpc-endpoint <RPC_ENDPOINT>` — Connect to remote RPC daemon (cli mode)
* `--rpc-listen <RPC_LISTEN>` — Listen address for RPC daemon (rpc mode)

  Default value: `0.0.0.0:9400`
* `--rpc-secret <RPC_SECRET>` — RPC encryption secret
* `--rest-listen <REST_LISTEN>` — Listen address for REST HTTP server (rest mode)

  Default value: `0.0.0.0:9401`
* `--rest-api-key <REST_API_KEY>` — API key for REST bearer authentication (rest mode)
* `--data-dir <DATA_DIR>` — Wallet and data directory
* `--output <OUTPUT>` — Output format

  Default value: `json`
* `--log <LOG>` — Log filters (comma-separated)
* `--dry-run` — Preview the command without executing it



## `afpay global`

Global (cross-network) operations

**Usage:** `afpay global <COMMAND>`

###### **Subcommands:**

* `limit` — Global spend limit (USD cents)
* `config` — Global runtime configuration



## `afpay global limit`

Global spend limit (USD cents)

**Usage:** `afpay global limit <COMMAND>`

###### **Subcommands:**

* `add` — Add a global spend limit (USD cents)



## `afpay global limit add`

Add a global spend limit (USD cents)

**Usage:** `afpay global limit add --window <WINDOW> --max-spend <MAX_SPEND>`

###### **Options:**

* `--window <WINDOW>` — Time window: e.g. 30m, 1h, 24h, 7d
* `--max-spend <MAX_SPEND>` — Maximum spend in USD cents



## `afpay global config`

Global runtime configuration

**Usage:** `afpay global config <COMMAND>`

###### **Subcommands:**

* `show` — Show current runtime configuration
* `set` — Update runtime configuration



## `afpay global config show`

Show current runtime configuration

**Usage:** `afpay global config show`



## `afpay global config set`

Update runtime configuration

**Usage:** `afpay global config set [OPTIONS]`

###### **Options:**

* `--log <LOG>` — Log filters (comma-separated: startup,cashu,ln,sol,wallet,all,off)



## `afpay cashu`

Cashu operations

**Usage:** `afpay cashu <COMMAND>`

###### **Subcommands:**

* `send` — Send P2P cashu token (outputs token string; for Lightning, use send-to-ln)
* `receive` — Receive cashu token
* `send-to-ln` — Send cashu to a Lightning invoice
* `receive-from-ln` — Create Lightning invoice to receive cashu from LN
* `receive-from-ln-claim` — Claim minted tokens from a receive-from-ln quote
* `balance` — Check cashu balance
* `wallet` — Wallet management
* `limit` — Spend limit for cashu network or a specific cashu wallet
* `config` — Per-wallet configuration



## `afpay cashu send`

Send P2P cashu token (outputs token string; for Lightning, use send-to-ln)

**Usage:** `afpay cashu send [OPTIONS] --amount-sats <AMOUNT_SATS>`

###### **Options:**

* `--amount-sats <AMOUNT_SATS>` — Amount in sats (base units)
* `--cashu-mint <MINT_URL>` — Restrict to wallets on these mint URLs (tried in order)
* `--wallet <WALLET>` — Source wallet ID (auto-selected if omitted)
* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo (sent with the transaction)
* `--local-memo <LOCAL_MEMO>` — Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)



## `afpay cashu receive`

Receive cashu token

**Usage:** `afpay cashu receive [OPTIONS] <TOKEN>`

###### **Arguments:**

* `<TOKEN>` — Cashu token string

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (auto-matched from token if omitted)



## `afpay cashu send-to-ln`

Send cashu to a Lightning invoice

**Usage:** `afpay cashu send-to-ln [OPTIONS] --to <TO>`

###### **Options:**

* `--to <TO>` — Lightning invoice (bolt11)
* `--wallet <WALLET>` — Source wallet ID (auto-selected if omitted)
* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo (sent with the transaction)
* `--local-memo <LOCAL_MEMO>` — Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)



## `afpay cashu receive-from-ln`

Create Lightning invoice to receive cashu from LN

**Usage:** `afpay cashu receive-from-ln [OPTIONS]`

###### **Options:**

* `--amount-sats <AMOUNT_SATS>` — Amount in sats (base units)
* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo (sent with the transaction)
* `--wallet <WALLET>` — Wallet ID (auto-selected if omitted)
* `--wait` — Wait for payment / matching receive transaction
* `--wait-timeout-s <WAIT_TIMEOUT_S>` — Timeout in seconds for --wait
* `--wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>` — Poll interval in milliseconds for --wait
* `--qr-svg-file` — Write receive QR payload to an SVG file

  Default value: `false`



## `afpay cashu receive-from-ln-claim`

Claim minted tokens from a receive-from-ln quote

**Usage:** `afpay cashu receive-from-ln-claim --wallet <WALLET> --ln-quote-id <LN_QUOTE_ID>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID
* `--ln-quote-id <LN_QUOTE_ID>` — Quote ID / payment hash from deposit



## `afpay cashu balance`

Check cashu balance

**Usage:** `afpay cashu balance [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit to show all cashu wallets)
* `--check` — Verify proofs against mint (slower but accurate)



## `afpay cashu wallet`

Wallet management

**Usage:** `afpay cashu wallet <COMMAND>`

###### **Subcommands:**

* `create` — Create a new cashu wallet
* `close` — Close a zero-balance cashu wallet
* `list` — List cashu wallets
* `dangerously-show-seed` — Dangerously show wallet seed mnemonic (12 BIP39 words)
* `restore` — Restore lost proofs from mint (fixes counter/proof sync issues)



## `afpay cashu wallet create`

Create a new cashu wallet

**Usage:** `afpay cashu wallet create [OPTIONS] --cashu-mint <MINT_URL>`

###### **Options:**

* `--cashu-mint <MINT_URL>` — Cashu mint URL
* `--label <LABEL>` — Optional label
* `--mnemonic-secret <MNEMONIC_SECRET>` — Existing BIP39 mnemonic secret to restore this wallet



## `afpay cashu wallet close`

Close a zero-balance cashu wallet

**Usage:** `afpay cashu wallet close [OPTIONS] --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID
* `--dangerously-skip-balance-check-and-may-lose-money` — Dangerously skip balance checks when closing wallet



## `afpay cashu wallet list`

List cashu wallets

**Usage:** `afpay cashu wallet list`



## `afpay cashu wallet dangerously-show-seed`

Dangerously show wallet seed mnemonic (12 BIP39 words)

**Usage:** `afpay cashu wallet dangerously-show-seed --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay cashu wallet restore`

Restore lost proofs from mint (fixes counter/proof sync issues)

**Usage:** `afpay cashu wallet restore --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay cashu limit`

Spend limit for cashu network or a specific cashu wallet

**Usage:** `afpay cashu limit [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `add` — Add a network or wallet spend limit

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit for network-level limit)



## `afpay cashu limit add`

Add a network or wallet spend limit

**Usage:** `afpay cashu limit add --window <WINDOW> --max-spend <MAX_SPEND>`

###### **Options:**

* `--window <WINDOW>` — Time window: e.g. 30m, 1h, 24h, 7d
* `--max-spend <MAX_SPEND>` — Maximum spend in base units



## `afpay cashu config`

Per-wallet configuration

**Usage:** `afpay cashu config --wallet <WALLET> <COMMAND>`

###### **Subcommands:**

* `show` — Show current wallet configuration
* `set` — Update wallet settings

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay cashu config show`

Show current wallet configuration

**Usage:** `afpay cashu config show`



## `afpay cashu config set`

Update wallet settings

**Usage:** `afpay cashu config set [OPTIONS]`

###### **Options:**

* `--label <LABEL>` — New label



## `afpay ln`

Lightning Network operations (NWC, phoenixd, LNbits)

**Usage:** `afpay ln <COMMAND>`

###### **Subcommands:**

* `wallet` — Wallet management
* `send` — Pay a Lightning invoice or BOLT12 offer
* `receive` — Create a Lightning invoice (BOLT11) or get a reusable BOLT12 offer
* `balance` — Check balance
* `limit` — Spend limit for ln network or a specific ln wallet
* `config` — Per-wallet configuration



## `afpay ln wallet`

Wallet management

**Usage:** `afpay ln wallet <COMMAND>`

###### **Subcommands:**

* `create` — Create a new Lightning wallet
* `close` — Close a Lightning wallet
* `list` — List Lightning wallets
* `dangerously-show-seed` — Dangerously show wallet seed (for LN this is backend credential, not mnemonic words)



## `afpay ln wallet create`

Create a new Lightning wallet

**Usage:** `afpay ln wallet create [OPTIONS] --backend <BACKEND>`

###### **Options:**

* `--backend <BACKEND>` — Backend: nwc, phoenixd, lnbits

  Possible values: `nwc`, `phoenixd`, `lnbits`

* `--nwc-uri-secret <NWC_URI_SECRET>` — NWC connection URI secret (for nwc backend)
* `--endpoint <ENDPOINT>` — Endpoint URL (for phoenixd, lnbits)
* `--password-secret <PASSWORD_SECRET>` — Password secret (for phoenixd)
* `--admin-key-secret <ADMIN_KEY_SECRET>` — Admin API key secret (for lnbits)
* `--label <LABEL>` — Optional label



## `afpay ln wallet close`

Close a Lightning wallet

**Usage:** `afpay ln wallet close [OPTIONS] --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID
* `--dangerously-skip-balance-check-and-may-lose-money` — Dangerously skip balance checks when closing wallet



## `afpay ln wallet list`

List Lightning wallets

**Usage:** `afpay ln wallet list`



## `afpay ln wallet dangerously-show-seed`

Dangerously show wallet seed (for LN this is backend credential, not mnemonic words)

**Usage:** `afpay ln wallet dangerously-show-seed --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay ln send`

Pay a Lightning invoice or BOLT12 offer

**Usage:** `afpay ln send [OPTIONS] --to <TO>`

###### **Options:**

* `--to <TO>` — BOLT11 invoice or BOLT12 offer (lno1…) to pay
* `--amount-sats <AMOUNT_SATS>` — Amount in sats (required for BOLT12 offers, rejected for BOLT11)
* `--wallet <WALLET>` — Source wallet ID (auto-selected if omitted)
* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo (sent with the transaction)
* `--local-memo <LOCAL_MEMO>` — Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)



## `afpay ln receive`

Create a Lightning invoice (BOLT11) or get a reusable BOLT12 offer

**Usage:** `afpay ln receive [OPTIONS]`

###### **Options:**

* `--amount-sats <AMOUNT_SATS>` — Amount in sats (omit for BOLT12 offer)
* `--wallet <WALLET>` — Wallet ID (auto-selected if omitted)
* `--wait` — Wait for payment / matching receive transaction
* `--wait-timeout-s <WAIT_TIMEOUT_S>` — Timeout in seconds for --wait
* `--wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>` — Poll interval in milliseconds for --wait
* `--qr-svg-file` — Write receive QR payload to an SVG file

  Default value: `false`



## `afpay ln balance`

Check balance

**Usage:** `afpay ln balance [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit to show all ln wallets)



## `afpay ln limit`

Spend limit for ln network or a specific ln wallet

**Usage:** `afpay ln limit [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `add` — Add a network or wallet spend limit

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit for network-level limit)



## `afpay ln limit add`

Add a network or wallet spend limit

**Usage:** `afpay ln limit add --window <WINDOW> --max-spend <MAX_SPEND>`

###### **Options:**

* `--window <WINDOW>` — Time window: e.g. 30m, 1h, 24h, 7d
* `--max-spend <MAX_SPEND>` — Maximum spend in base units



## `afpay ln config`

Per-wallet configuration

**Usage:** `afpay ln config --wallet <WALLET> <COMMAND>`

###### **Subcommands:**

* `show` — Show current wallet configuration
* `set` — Update wallet settings

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay ln config show`

Show current wallet configuration

**Usage:** `afpay ln config show`



## `afpay ln config set`

Update wallet settings

**Usage:** `afpay ln config set [OPTIONS]`

###### **Options:**

* `--label <LABEL>` — New label



## `afpay sol`

Solana operations

**Usage:** `afpay sol <COMMAND>`

###### **Subcommands:**

* `wallet` — Wallet management
* `send` — Send SOL or SPL token transfer
* `receive` — Show wallet receive address
* `balance` — Check balance
* `limit` — Spend limit for sol network or a specific sol wallet
* `config` — Per-wallet configuration



## `afpay sol wallet`

Wallet management

**Usage:** `afpay sol wallet <COMMAND>`

###### **Subcommands:**

* `create` — Create a new Solana wallet
* `close` — Close a Solana wallet
* `list` — List Solana wallets
* `dangerously-show-seed` — Dangerously show wallet seed mnemonic (12 BIP39 words)



## `afpay sol wallet create`

Create a new Solana wallet

**Usage:** `afpay sol wallet create [OPTIONS] --sol-rpc-endpoint <SOL_RPC_ENDPOINT>`

###### **Options:**

* `--sol-rpc-endpoint <SOL_RPC_ENDPOINT>` — Solana JSON-RPC endpoint (repeat to configure failover order)
* `--label <LABEL>` — Optional label



## `afpay sol wallet close`

Close a Solana wallet

**Usage:** `afpay sol wallet close [OPTIONS] --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID
* `--dangerously-skip-balance-check-and-may-lose-money` — Dangerously skip balance checks when closing wallet



## `afpay sol wallet list`

List Solana wallets

**Usage:** `afpay sol wallet list`



## `afpay sol wallet dangerously-show-seed`

Dangerously show wallet seed mnemonic (12 BIP39 words)

**Usage:** `afpay sol wallet dangerously-show-seed --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay sol send`

Send SOL or SPL token transfer

**Usage:** `afpay sol send [OPTIONS] --to <TO> --amount <AMOUNT> --token <TOKEN>`

###### **Options:**

* `--to <TO>` — Recipient Solana address (base58)
* `--amount <AMOUNT>` — Amount in token base units (lamports for SOL, smallest unit for SPL tokens)
* `--token <TOKEN>` — Token: "native" for SOL, "usdc", "usdt", or SPL mint address
* `--reference <REFERENCE>` — Reference key for order binding (base58-encoded 32 bytes, per strain-payment-method-solana)
* `--wallet <WALLET>` — Source wallet ID (auto-selected if omitted)
* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo (sent with the transaction)
* `--local-memo <LOCAL_MEMO>` — Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)



## `afpay sol receive`

Show wallet receive address

**Usage:** `afpay sol receive [OPTIONS]`

###### **Options:**

* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo to watch for (used with --wait)
* `--min-confirmations <MIN_CONFIRMATIONS>` — Minimum confirmation depth before considering payment settled (requires --wait)
* `--reference <REFERENCE>` — Reference key to watch for (base58, used with --wait, per strain-payment-method-solana)
* `--wallet <WALLET>` — Wallet ID (auto-selected if omitted)
* `--wait` — Wait for payment / matching receive transaction
* `--wait-timeout-s <WAIT_TIMEOUT_S>` — Timeout in seconds for --wait
* `--wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>` — Poll interval in milliseconds for --wait
* `--qr-svg-file` — Write receive QR payload to an SVG file

  Default value: `false`



## `afpay sol balance`

Check balance

**Usage:** `afpay sol balance [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit to show all sol wallets)



## `afpay sol limit`

Spend limit for sol network or a specific sol wallet

**Usage:** `afpay sol limit [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `add` — Add a network or wallet spend limit

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit for network-level limit)



## `afpay sol limit add`

Add a network or wallet spend limit

**Usage:** `afpay sol limit add [OPTIONS] --window <WINDOW> --max-spend <MAX_SPEND>`

###### **Options:**

* `--token <TOKEN>` — Token: native, usdc, usdt
* `--window <WINDOW>` — Time window: e.g. 30m, 1h, 24h, 7d
* `--max-spend <MAX_SPEND>` — Maximum spend in base units



## `afpay sol config`

Per-wallet configuration

**Usage:** `afpay sol config --wallet <WALLET> <COMMAND>`

###### **Subcommands:**

* `show` — Show current wallet configuration
* `set` — Update wallet settings
* `token-add` — Register a custom token for balance tracking
* `token-remove` — Unregister a custom token

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay sol config show`

Show current wallet configuration

**Usage:** `afpay sol config show`



## `afpay sol config set`

Update wallet settings

**Usage:** `afpay sol config set [OPTIONS]`

###### **Options:**

* `--label <LABEL>` — New label
* `--rpc-endpoint <RPC_ENDPOINT>` — Replace RPC endpoint(s)



## `afpay sol config token-add`

Register a custom token for balance tracking

**Usage:** `afpay sol config token-add [OPTIONS] --symbol <SYMBOL> --address <ADDRESS>`

###### **Options:**

* `--symbol <SYMBOL>` — Token symbol (e.g. dai)
* `--address <ADDRESS>` — Token contract address
* `--decimals <DECIMALS>` — Token decimals

  Default value: `6`



## `afpay sol config token-remove`

Unregister a custom token

**Usage:** `afpay sol config token-remove --symbol <SYMBOL>`

###### **Options:**

* `--symbol <SYMBOL>` — Token symbol to remove



## `afpay evm`

EVM chain operations (Base, Arbitrum)

**Usage:** `afpay evm <COMMAND>`

###### **Subcommands:**

* `wallet` — Wallet management
* `send` — Send native token or ERC-20 token transfer
* `receive` — Show wallet receive address
* `balance` — Check balance
* `limit` — Spend limit for evm network or a specific evm wallet
* `config` — Per-wallet configuration



## `afpay evm wallet`

Wallet management

**Usage:** `afpay evm wallet <COMMAND>`

###### **Subcommands:**

* `create` — Create a new EVM chain wallet
* `close` — Close an EVM chain wallet
* `list` — List EVM chain wallets
* `dangerously-show-seed` — Dangerously show wallet seed mnemonic (12 BIP39 words)



## `afpay evm wallet create`

Create a new EVM chain wallet

**Usage:** `afpay evm wallet create [OPTIONS] --evm-rpc-endpoint <EVM_RPC_ENDPOINT>`

###### **Options:**

* `--evm-rpc-endpoint <EVM_RPC_ENDPOINT>` — EVM JSON-RPC endpoint (repeat to configure failover order)
* `--chain-id <CHAIN_ID>` — Chain ID (default: 8453 = Base)

  Default value: `8453`
* `--label <LABEL>` — Optional label



## `afpay evm wallet close`

Close an EVM chain wallet

**Usage:** `afpay evm wallet close [OPTIONS] --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID
* `--dangerously-skip-balance-check-and-may-lose-money` — Dangerously skip balance checks when closing wallet



## `afpay evm wallet list`

List EVM chain wallets

**Usage:** `afpay evm wallet list`



## `afpay evm wallet dangerously-show-seed`

Dangerously show wallet seed mnemonic (12 BIP39 words)

**Usage:** `afpay evm wallet dangerously-show-seed --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay evm send`

Send native token or ERC-20 token transfer

**Usage:** `afpay evm send [OPTIONS] --to <TO> --amount <AMOUNT> --token <TOKEN>`

###### **Options:**

* `--to <TO>` — Recipient address (0x...)
* `--amount <AMOUNT>` — Amount in token base units (wei for ETH, smallest unit for ERC-20)
* `--token <TOKEN>` — Token: "native" for chain native, "usdc" or contract address for ERC-20
* `--wallet <WALLET>` — Source wallet ID (auto-selected if omitted)
* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo (sent with the transaction)
* `--local-memo <LOCAL_MEMO>` — Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)



## `afpay evm receive`

Show wallet receive address

**Usage:** `afpay evm receive [OPTIONS]`

###### **Options:**

* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo to watch for (used with --wait)
* `--min-confirmations <MIN_CONFIRMATIONS>` — Minimum confirmation depth before considering payment settled (requires --wait)
* `--wallet <WALLET>` — Wallet ID (auto-selected if omitted)
* `--wait` — Wait for payment / matching receive transaction
* `--wait-timeout-s <WAIT_TIMEOUT_S>` — Timeout in seconds for --wait
* `--wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>` — Poll interval in milliseconds for --wait
* `--qr-svg-file` — Write receive QR payload to an SVG file

  Default value: `false`



## `afpay evm balance`

Check balance

**Usage:** `afpay evm balance [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit to show all evm wallets)



## `afpay evm limit`

Spend limit for evm network or a specific evm wallet

**Usage:** `afpay evm limit [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `add` — Add a network or wallet spend limit

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit for network-level limit)



## `afpay evm limit add`

Add a network or wallet spend limit

**Usage:** `afpay evm limit add [OPTIONS] --window <WINDOW> --max-spend <MAX_SPEND>`

###### **Options:**

* `--token <TOKEN>` — Token: native, usdc, usdt
* `--window <WINDOW>` — Time window: e.g. 30m, 1h, 24h, 7d
* `--max-spend <MAX_SPEND>` — Maximum spend in base units



## `afpay evm config`

Per-wallet configuration

**Usage:** `afpay evm config --wallet <WALLET> <COMMAND>`

###### **Subcommands:**

* `show` — Show current wallet configuration
* `set` — Update wallet settings
* `token-add` — Register a custom token for balance tracking
* `token-remove` — Unregister a custom token

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay evm config show`

Show current wallet configuration

**Usage:** `afpay evm config show`



## `afpay evm config set`

Update wallet settings

**Usage:** `afpay evm config set [OPTIONS]`

###### **Options:**

* `--label <LABEL>` — New label
* `--rpc-endpoint <RPC_ENDPOINT>` — Replace RPC endpoint(s)
* `--chain-id <CHAIN_ID>` — EVM chain ID



## `afpay evm config token-add`

Register a custom token for balance tracking

**Usage:** `afpay evm config token-add [OPTIONS] --symbol <SYMBOL> --address <ADDRESS>`

###### **Options:**

* `--symbol <SYMBOL>` — Token symbol (e.g. dai)
* `--address <ADDRESS>` — Token contract address
* `--decimals <DECIMALS>` — Token decimals

  Default value: `6`



## `afpay evm config token-remove`

Unregister a custom token

**Usage:** `afpay evm config token-remove --symbol <SYMBOL>`

###### **Options:**

* `--symbol <SYMBOL>` — Token symbol to remove



## `afpay btc`

Bitcoin on-chain operations

**Usage:** `afpay btc <COMMAND>`

###### **Subcommands:**

* `wallet` — Wallet management
* `send` — Send BTC on-chain
* `receive` — Show wallet receive address
* `balance` — Check balance
* `limit` — Spend limit for btc network or a specific btc wallet
* `config` — Per-wallet configuration



## `afpay btc wallet`

Wallet management

**Usage:** `afpay btc wallet <COMMAND>`

###### **Subcommands:**

* `create` — Create a new Bitcoin wallet
* `close` — Close a Bitcoin wallet
* `list` — List Bitcoin wallets
* `dangerously-show-seed` — Dangerously show wallet seed mnemonic (12 BIP39 words)



## `afpay btc wallet create`

Create a new Bitcoin wallet

**Usage:** `afpay btc wallet create [OPTIONS]`

###### **Options:**

* `--btc-network <BTC_NETWORK>` — Bitcoin sub-network: mainnet or signet (default: mainnet)

  Default value: `mainnet`
* `--btc-address-type <BTC_ADDRESS_TYPE>` — Address type: taproot or segwit (default: taproot)

  Default value: `taproot`
* `--btc-backend <BTC_BACKEND>` — Chain-source backend: esplora (default), core-rpc, electrum

  Possible values: `esplora`, `core-rpc`, `electrum`

* `--btc-esplora-url <BTC_ESPLORA_URL>` — Custom Esplora API URL
* `--btc-core-url <BTC_CORE_URL>` — Bitcoin Core RPC URL (core-rpc backend)
* `--btc-core-auth-secret <BTC_CORE_AUTH_SECRET>` — Bitcoin Core RPC auth "user:pass" (core-rpc backend)
* `--btc-electrum-url <BTC_ELECTRUM_URL>` — Electrum server URL (electrum backend)
* `--mnemonic-secret <MNEMONIC_SECRET>` — Existing BIP39 mnemonic secret to restore wallet
* `--label <LABEL>` — Optional label



## `afpay btc wallet close`

Close a Bitcoin wallet

**Usage:** `afpay btc wallet close [OPTIONS] --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID
* `--dangerously-skip-balance-check-and-may-lose-money` — Dangerously skip balance checks when closing wallet



## `afpay btc wallet list`

List Bitcoin wallets

**Usage:** `afpay btc wallet list`



## `afpay btc wallet dangerously-show-seed`

Dangerously show wallet seed mnemonic (12 BIP39 words)

**Usage:** `afpay btc wallet dangerously-show-seed --wallet <WALLET>`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay btc send`

Send BTC on-chain

**Usage:** `afpay btc send [OPTIONS] --to <TO> --amount-sats <AMOUNT_SATS>`

###### **Options:**

* `--to <TO>` — Recipient Bitcoin address (bc1.../tb1...)
* `--amount-sats <AMOUNT_SATS>` — Amount in satoshis
* `--wallet <WALLET>` — Source wallet ID (auto-selected if omitted)
* `--onchain-memo <ONCHAIN_MEMO>` — On-chain memo (sent with the transaction)
* `--local-memo <LOCAL_MEMO>` — Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)



## `afpay btc receive`

Show wallet receive address

**Usage:** `afpay btc receive [OPTIONS]`

###### **Options:**

* `--wait-sync-limit <WAIT_SYNC_LIMIT>` — Max history records scanned per poll when resolving tx id
* `--wallet <WALLET>` — Wallet ID (auto-selected if omitted)
* `--wait` — Wait for payment / matching receive transaction
* `--wait-timeout-s <WAIT_TIMEOUT_S>` — Timeout in seconds for --wait
* `--wait-poll-interval-ms <WAIT_POLL_INTERVAL_MS>` — Poll interval in milliseconds for --wait
* `--qr-svg-file` — Write receive QR payload to an SVG file

  Default value: `false`



## `afpay btc balance`

Check balance

**Usage:** `afpay btc balance [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit to show all btc wallets)



## `afpay btc limit`

Spend limit for btc network or a specific btc wallet

**Usage:** `afpay btc limit [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `add` — Add a network or wallet spend limit

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit for network-level limit)



## `afpay btc limit add`

Add a network or wallet spend limit

**Usage:** `afpay btc limit add --window <WINDOW> --max-spend <MAX_SPEND>`

###### **Options:**

* `--window <WINDOW>` — Time window: e.g. 30m, 1h, 24h, 7d
* `--max-spend <MAX_SPEND>` — Maximum spend in base units



## `afpay btc config`

Per-wallet configuration

**Usage:** `afpay btc config --wallet <WALLET> <COMMAND>`

###### **Subcommands:**

* `show` — Show current wallet configuration
* `set` — Update wallet settings

###### **Options:**

* `--wallet <WALLET>` — Wallet ID



## `afpay btc config show`

Show current wallet configuration

**Usage:** `afpay btc config show`



## `afpay btc config set`

Update wallet settings

**Usage:** `afpay btc config set [OPTIONS]`

###### **Options:**

* `--label <LABEL>` — New label



## `afpay wallet`

List all wallets (cross-network)

**Usage:** `afpay wallet <COMMAND>`

###### **Subcommands:**

* `list` — List all wallets (cross-network)



## `afpay wallet list`

List all wallets (cross-network)

**Usage:** `afpay wallet list [OPTIONS]`

###### **Options:**

* `--network <NETWORK>` — Filter by network: cashu, ln, sol, evm

  Possible values: `ln`, `sol`, `evm`, `cashu`, `btc`




## `afpay balance`

All wallets balance (cross-network)

**Usage:** `afpay balance [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Wallet ID (omit to show all wallets)
* `--network <NETWORK>` — Filter by network: cashu, ln, sol, evm

  Possible values: `ln`, `sol`, `evm`, `cashu`, `btc`

* `--cashu-check` — Verify cashu proofs against mint (slower but accurate; cashu only)



## `afpay history`

History queries

**Usage:** `afpay history <COMMAND>`

###### **Subcommands:**

* `list` — List history records from local store
* `status` — Check history status
* `update` — Incrementally sync on-chain/backend history into local store



## `afpay history list`

List history records from local store

**Usage:** `afpay history list [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Filter by wallet ID
* `--network <NETWORK>` — Filter by network: cashu, ln, sol, evm

  Possible values: `ln`, `sol`, `evm`, `cashu`, `btc`

* `--onchain-memo <ONCHAIN_MEMO>` — Filter by exact on-chain memo text
* `--limit <LIMIT>` — Max results

  Default value: `20`
* `--offset <OFFSET>` — Offset

  Default value: `0`
* `--since-epoch-s <SINCE_EPOCH_S>` — Only include records created at or after this epoch second
* `--until-epoch-s <UNTIL_EPOCH_S>` — Only include records created before this epoch second



## `afpay history status`

Check history status

**Usage:** `afpay history status --transaction-id <TRANSACTION_ID>`

###### **Options:**

* `--transaction-id <TRANSACTION_ID>` — Transaction ID



## `afpay history update`

Incrementally sync on-chain/backend history into local store

**Usage:** `afpay history update [OPTIONS]`

###### **Options:**

* `--wallet <WALLET>` — Sync a specific wallet (defaults to all wallets in scope)
* `--network <NETWORK>` — Restrict sync to a single network

  Possible values: `ln`, `sol`, `evm`, `cashu`, `btc`

* `--limit <LIMIT>` — Max records to scan per wallet during this incremental sync

  Default value: `200`



## `afpay limit`

Spend limit list and remove (cross-network)

**Usage:** `afpay limit <COMMAND>`

###### **Subcommands:**

* `remove` — Remove a spend limit rule by ID
* `list` — List current limit status



## `afpay limit remove`

Remove a spend limit rule by ID

**Usage:** `afpay limit remove --rule-id <RULE_ID>`

###### **Options:**

* `--rule-id <RULE_ID>` — Rule ID (e.g. r_1a2b3c4d)



## `afpay limit list`

List current limit status

**Usage:** `afpay limit list`



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>
