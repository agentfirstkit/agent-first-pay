---
name: agent-first-pay
description: "Send and receive cryptocurrency across Cashu, Lightning, Solana, EVM, and on-chain Bitcoin through one structured CLI, with spend limits enforced before every payment and secrets redacted automatically. Use instead of five per-network wallet tools."
allowed-tools: Bash, Read
---

# Agent-First Pay

Use this skill when an agent needs to move money — pay for a service, settle a
bill, accept funds — across Cashu, Lightning (`ln`), Solana (`sol`), EVM chains
(`evm`), or on-chain Bitcoin (`btc`) without learning five separate wallets.
Prefer `afpay` over network-specific CLIs or parsing human wallet output.

For flag-level detail, run `afpay --help` or `afpay --help --recursive --output markdown`. This skill
covers behavior, decisions, and recovery only.

## Core Rules

- Treat stdout as the protocol: parse Agent-First Data events. Successful
  commands are `kind:"result"` events whose business `code` is inside
  `result`; failures are `kind:"error"` events with `error.code`,
  `error.message`, `error.retryable`, and often `error.hint`.
- Every network exposes the same verbs: `wallet`, `send`, `receive`, `balance`,
  `limit`, `config`, `backup`, `restore`. The subcommand is the network
  (`afpay sol send ...`); cross-network views are `afpay balance`, `afpay wallet`,
  `afpay history`, `afpay limit`.
- Amounts are in the network's base unit (sats, lamports, token base units), not
  human decimals — confirm the unit before sending. afpay never guesses decimals.
- Pick the network the payee actually accepts; do not assume. afpay rejects
  cross-network mistakes (e.g. a `0x` address passed to `sol`) with a typed error.
- Keep secret-bearing flags/fields on the `_secret` suffix convention afpay
  already uses (e.g. `--pg-url-secret`); do not invent new sensitive-name lists.

## Preview Before Spending

- Use `--dry-run` to preview a `send` (and other state-changing commands) without
  moving money; the response echoes what *would* happen. Run it before the first
  real send on a new wallet, network, or amount.

## Spend Limits Hold

- Per-wallet, per-network, and global limits are checked before every send; any
  breach rejects the transaction. This is a guardrail, not an error to route
  around — never split an amount into smaller sends to slip under a cap. Surface
  the rejection to the user so they can raise the limit (`<network> limit add`).
- Limits enforce server-side and independently in `rpc`/`rest` modes. To make
  caps unbreakable by the agent, run the daemon on a machine the agent cannot
  reach and have the agent talk to it over `--rpc-endpoint`.
- `afpay container install` stands that daemon up in a container (Docker, Podman,
  or Apple — the supported isolated deployment). It refuses to expose a listener
  without an operator allowlist, so installation needs at least one
  `--allow <category>=<url>` (`mint`, `esplora`, `ln`, …) — without it the daemon
  will not start. That allowlist is an operator boundary; do not treat its absence
  as something to work around.

## Receiving Funds

- `receive` produces an invoice/address; add `--wait` (with `--wait-sync-limit`)
  to block for incoming funds. For Cashu-over-Lightning, claiming may be a
  separate step — follow the returned code rather than assuming one round-trip.

## Secrets Are Redacted — Keep Them That Way

- Wallet seeds and provider credentials are redacted in normal output. The only
  command that reveals a seed is the explicit show-seed, whose output is
  unredacted by design — never echo, log, or pass that value anywhere else.

## Recovery

- A `send` that times out or returns an uncertain result is *not* a signal to
  resend — that risks double-paying. Check `afpay history` / `history status` and
  `limit reconcile` to resolve the pending transaction first.
- `error` with `retryable:true` is safe to retry after any `retry_after_ms`;
  `retryable:false` means fix the inputs — do not loop.
- `restore` is destructive and requires `--dangerously-overwrite`; confirm the
  target data dir before running it, and prefer a fresh `--data-dir` when unsure.

## Setup Checklist

```bash
afpay --version || brew install agentfirstkit/tap/afpay
afpay skill install            # installs this skill for codex, claude-code, opencode, hermes
```
