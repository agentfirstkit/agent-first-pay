---
name: agent-first-pay
description: "Send and receive cryptocurrency across Cashu, Lightning, Solana, EVM, and on-chain Bitcoin through one structured CLI, with spend limits checked before every payment and secrets redacted automatically. Use instead of five per-network wallet tools."
allowed-tools: Bash, Read
---

# Agent-First Pay

Use this skill when an agent needs to move money — pay for a service, settle a
bill, accept funds — across Cashu, Lightning (`ln`), Solana (`sol`), EVM chains
(`evm`), or on-chain Bitcoin (`btc`) without learning five separate wallets.
Prefer `afpay` over network-specific CLIs or parsing human wallet output.

For flag-level detail, ask the command itself: `afpay <command> --help` returns
every legal shape of that call at once, plus the ready-to-run `--help` line for
each subcommand. One call is enough — there is no recursive mode. Add
`--output plain` for a terminal-shaped rendering. This skill covers behavior,
decisions, and recovery only.

Arguments are command-local and always follow the whole command path:
`afpay cashu send --amount-sats 100 --data-dir /srv/afpay`, never
`afpay --data-dir /srv/afpay cashu send …`. The two long-lived server and
session modes are the exception: `--mode` and its listener flags belong to bare
`afpay`.

## Core Rules

- Treat stdout as the protocol: parse Agent-First Data events. Successful
  commands are `kind:"result"` events whose business `code` is inside
  `result`; failures are `kind:"error"` events with `error.code`,
  `error.message`, `error.retryable`, and often `error.hint`.
- A rejected invocation names its own rule in `error.code` — `cli_unknown_argument`,
  `cli_unknown_command`, `cli_unregistered_combination`, `cli_invalid_argument_value`,
  and their siblings. `cli_unregistered_combination` means the flags are each
  valid but not legal *together*: read the shapes in `--help` and pick one,
  do not retry with more flags.
- Every network exposes the same verbs: `wallet`, `send`, `receive`, `balance`,
  `limit`, `config`, `backup`, `restore`. The subcommand is the network
  (`afpay sol send ...`); cross-network views are `afpay balance`, `afpay wallet`,
  `afpay history`, `afpay limit`.
- `afpay ui …` is not a second way to ask afpay anything. Each panel runs the
  same request as the command it mirrors, then opens a window on the person's
  machine and does not return until they are done with it. Call it only when you
  want a human involved; when you need the answer yourself, call the command —
  the same request, the same numbers, and it exits.
- `afpay ui send` is the panel that asks a *person* before moving money. It
  blocks until they answer, and only an approval sends: a closed window is a
  refusal. Read `decision` and `dispatched` off the terminal result rather than
  inferring either. A refusal is an answer — do not re-issue it as a `send`
  plus a `pay confirm`.
- `afpay ui receive` shows what to scan; it does not watch for the payment.
  When *you* need to know the money arrived, use `<network> receive --wait`.
- A panel reaches the person three ways, and which are on offer differs by what
  the panel does. The two watch panels offer all three, including the LAN link —
  a receive code is *for* pointing a phone at. `afpay ui send` does not: that
  link URL is a bearer capability, and what it would bear is the approval of
  money leaving. To answer a send from another device, deliver it as `session`
  and open it through `afui session serve`. Do not choose a delivery on the
  person's behalf when they have not asked for one — leave `--mode` off and
  `AFUI_DELIVERY` decides, which is how an unattended machine avoids having a
  window opened on it.
- Any panel can be replaced with `afui frontend` (`ui_api_version` `1`; see
  `docs/architecture.md`). After installing one, read `ui_frontend_id` off the
  `ui_ready` progress event: present means your override is serving, absent
  means afpay's own page is — a workspace frontend nobody enabled is skipped in
  silence, and that field is the only way to tell it apart from a live one. A
  frontend afpay cannot load is a `ui_frontend_*` error and no window at all;
  fix it or set `AFUI_SAFE_MODE=1`, never assume it fell back.
- Writing a `send_confirm` template: restructure it freely, but declare both
  controls with `data-afpay-decision="approve"` and `="refuse"` — afpay binds
  them, the page does not, and a page missing either one refuses to open.
- Amounts are in the network's base unit (sats, lamports, token base units), not
  human decimals — confirm the unit before sending. afpay never guesses decimals.
- Pick the network the payee actually accepts; do not assume. afpay rejects
  cross-network mistakes (e.g. a `0x` address passed to `sol`) with a typed error.
- Keep secret-bearing flags/fields on the `_secret` suffix convention afpay
  already uses (e.g. `--pg-url-secret`); do not invent new sensitive-name lists.

## Paying Is Two Commands

- `<network> send` **does not pay**. It resolves the payment and returns
  `pay_planned`: the wallet afpay picked, `amount_native`,
  `fee_estimate_native`, `fee_unit`, the `spend_debits` it would consume, and a
  `plan_id`. Read those numbers and every item in `warnings` before going
  further — warnings are part of the result so log filters cannot hide them.
  This is the only point at which refusing is free.
- `afpay pay confirm --plan-id <id>` is what pays, and it is the only command
  that does. Pass `--idempotency-key` on every confirm.
- A plan is single-use and expires (`expires_at_epoch_ms`). Confirming a spent
  one answers `plan_not_found`; that is not a reason to re-plan and pay again
  unless you know the first confirm never ran — check `afpay history` first.
- `plan_stale` means the workspace, daemon configuration, wallet, or spend
  rules changed after the plan was resolved. `error.hint` names which. Resolve
  a new plan and read it again; never treat this as a transient failure to
  retry through.
- Do not carry a `plan_id` between machines or workspaces. It is bound to the
  one that issued it and will not be found anywhere else.
- Use `--dry-run` to preview a command without executing it; the response
  echoes what *would* happen. For a payment the plan already does this, so
  `--dry-run` is for the rest of the surface.

## Spend Limits Hold

- Per-wallet, per-network, and global limits are checked before every send; any
  breach rejects the transaction. This is a guardrail, not an error to route
  around — never split an amount into smaller sends to slip under a cap. Surface
  the rejection to the user so they can raise the limit (`<network> limit add`).
- **What a limit holds is the planned amount plus an estimated fee, not the
  final charge.** Network fees are estimated when the plan is made and the
  network charges what it charges at execution — a gas price that moves, a
  different UTXO set, a fresh melt quote, an account that has to be created.
  Treat the reviewed total as close, not as a ceiling, and do not design a
  workflow that depends on a limit being exact to the last unit.
- A `--mode rest` daemon enforces limits server-side, and every node in a chain
  enforces its own. To make caps unbreakable by the agent, run the daemon on a
  machine the agent cannot reach and talk to it over `--peer-url` with that
  node's `--peer-api-key-secret`.
- The peer withholds what it withholds. Seeds, spend-limit rule writes,
  reservation reconcile, and wallet-config writes are refused over `--peer-url`
  exactly as they are over HTTP; a `forbidden` answer there means "run it on the
  daemon's host", never "find another route".
- A `peer_unreachable`, `peer_not_afpay`, `peer_route_unsupported`,
  `peer_unauthorized`, or `peer_mismatch` error is a configuration fault, not a
  transient one. Report it as-is; only `peer_unreachable` is worth retrying.
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
- `receive` and `wallet create` take `--idempotency-key` too, and for the same
  reason a payment does: a repeat mints a second invoice a payer may not be
  looking at, or a second wallet you cannot tell from the first. Pass one and
  re-send it verbatim on retry.

## Secrets Are Redacted — Keep Them That Way

- Wallet seeds and provider credentials are redacted in normal output. The only
  command that reveals a seed is the explicit show-seed, whose output is
  unredacted by design — never echo, log, or pass that value anywhere else.
- **A `backup` archive is not redacted.** It contains wallet seed secrets in the
  clear, wallet data, and any database dump — it is the wallet, in a file. It
  carries no encryption and no signature of its own, so treat it exactly like a
  seed: encrypt it before it leaves the host that made it, and never `restore`
  an archive whose origin you cannot vouch for.

## Recovery

- A `pay confirm` that times out or returns an uncertain result is *not* a
  signal to plan again — that risks double-paying. Re-send the **same**
  `--idempotency-key` with the same `--plan-id`: if the first one ran, you get
  its original result back. Only if that answers `plan_not_found` do you check
  `afpay history` / `history status` and `limit reconcile` before deciding
  anything.
- `idempotency_conflict` means that key was already used for a *different*
  plan. Pick a new key; do not retry with the same one.
- `accounting_inconsistent` means the network side effect happened but one or
  more spend-ledger reservations did not commit. It is the sole terminal
  outcome, names the transaction and reservations, and is replayed by the same
  idempotency key. Never retry or re-plan; run `limit reconcile` locally after
  verifying the transaction.
- `error` with `retryable:true` is safe to retry after any `retry_after_ms`
  **for anything before a `pay confirm`**. After a confirm has been sent, a
  network error is not a statement that nothing happened: the transaction may
  have been broadcast, the mint may have advanced, the backend may have paid,
  and only the reply was lost. Re-send the same `--idempotency-key` with the
  same `--plan-id` as above, and if that cannot answer, check `afpay history`
  and the chain or backend before doing anything else. Never plan again on the
  strength of `retryable:true` alone. `retryable:false` means fix the inputs —
  do not loop.
- `restore` is destructive and requires `--dangerously-overwrite`; confirm the
  target data dir before running it, and prefer a fresh `--data-dir` when unsure.

## Setup Checklist

```bash
afpay --version || brew install agentfirstkit/tap/afpay
afpay skill install            # installs this skill for codex, claude-code, opencode, hermes
```
