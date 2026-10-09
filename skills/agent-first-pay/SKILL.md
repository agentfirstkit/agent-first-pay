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
  When *you* need to know the money arrived, check
  `afpay <network> receive --help`: not every network can wait for receipt.
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
  unless you know the first confirm never ran — refresh with
  `afpay history update`, inspect `afpay history list`, and check the provider.
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

- An invoice or address is a request for payment, not proof of receipt. Not
  every network can wait, and the available matching conditions differ; read
  `afpay <network> receive --help` before choosing a waiting shape. Cashu token
  redemption is a separate operation from requesting a Lightning deposit and
  claiming its proofs; follow the returned code.
- Pass an idempotency key to wallet creation and invoice creation wherever
  their help offers one, then preserve the exact arguments and key on retry.
  A repeat without a key can create a second wallet or invoice. Do not assume
  token redemption or quote claiming exposes the same retry controls.

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

- After a `pay confirm` timeout, preserve the **same** `--idempotency-key`,
  `--plan-id`, arguments, and workspace. One identical retry can recover a
  stored terminal receipt; it cannot prove that an unfinished submission
  failed. `idempotency_in_progress` means the key is still Pending: stop
  submitting and inspect provider or chain history. Do not poll the key
  indefinitely, change keys, or create a fresh plan to retry that payment.
  `plan_not_found` also requires checking history; a consumed plan does not
  prove whether payment succeeded.
- `idempotency_conflict` means that key was already used for a *different*
  plan. Stop and inspect the original payment and the intended plan; changing
  the key automatically can duplicate a payment.
- `accounting_inconsistent` covers two outcomes. A known successful payment
  can have failed spend-ledger confirmation; an unknown submission means the
  provider may have completed the payment but afpay has no confirmed receipt.
  Read `confirm_errors` and `hint`, then verify against provider or chain
  history before reporting payment status. A `transaction_id` may be present
  for either outcome or absent for an unknown submission, and
  `reservation_ids` may be empty. Neither field alone proves success.
  Use `afpay history status` when a recovery ID is available, and refresh with
  `afpay history update` before trusting `afpay history list`. Missing local
  history alone does not prove that no payment happened.
- A known success with an accounting failure is replayable if its terminal
  receipt was stored. An unknown submission consumes the plan but keeps the
  key Pending, so the same key answers `idempotency_in_progress`, not a replay.
  Neither outcome authorizes another payment. Run `limit reconcile` locally
  only after verifying whether funds moved. Reconcile repairs spend-budget
  reservations; it does not clear a Pending idempotency key or create a receipt.
- `sent` or `cashu_sent` with `idempotency_finalize_failed: true` is a
  successful payment whose replay receipt could not be stored at one or more
  nodes. Keep that receipt and repair the affected node's local record; do
  not repeat the payment even if an identical retry cannot replay it.
- `error` with `retryable:true` is safe to retry after any `retry_after_ms`.
  That holds for a `pay confirm` too, because of what afpay answers there: a
  `network_error` **returned by** a confirm means the payment was not
  submitted — nothing was signed, broadcast, or handed to a backend, the plan
  was given back, and the key was cleared. Retry with the **same** `--plan-id`
  and the **same** `--idempotency-key`; do not plan again or change the key.
  `retryable:false` means fix the inputs — do not loop.
- An outcome afpay cannot establish is never reported as `network_error`. Once
  submission may have begun, a lost or unreadable provider reply — including a
  success status whose body does not identify the payment — surfaces as
  `accounting_inconsistent` with the plan consumed and the key Pending. That
  is the unknown-submission case above: verify against provider or chain
  history and never retry it blindly. The same applies when the confirm itself
  got no answer at all (a timeout or dropped connection on your side): that is
  not a `network_error` from afpay, so follow the timeout rule above.
- On-chain Bitcoin asks the wallet's chain source for a fee rate at a
  roughly six-block target, at plan time and again at confirm. If the source
  has no estimate, or returns one above afpay's fixed safety ceiling
  (1000 sat/vB — a guard against a faulty or hijacked source, not a fee
  policy), the payment fails with a retryable `network_error` before signing
  and nothing is broadcast. Retry later or point the wallet at a healthy
  chain source; there is no flag to override the ceiling.
- `restore` is destructive and requires `--dangerously-overwrite`; confirm the
  target data dir before running it, and prefer a fresh `--data-dir` when unsure.

## Setup Checklist

```bash
afpay --version || brew install agentfirstkit/tap/afpay
afpay skill install            # installs this skill for codex, claude-code, opencode, hermes
```
