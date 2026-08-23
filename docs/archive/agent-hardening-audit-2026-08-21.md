# Agent-Hardening Audit (historical)

> Closed 2026-08-21. This is evidence and design history, not an active work
> queue. The final review kept the in-flight cap but rejected a generic cancel
> after a payment may have reached a provider; treated the single-use claimed
> plan as the fail-closed boundary for the idempotency crash window; kept error
> codes as an enumerated string contract; and declined reservation enumeration
> until a read/recovery consumer needs more than the IDs already returned by
> payment and inconsistency results. Structured plan warnings replaced the SOL
> hostname hard refusal, the wire schema now identifies its build, and ledger
> confirmation failure now emits one `accounting_inconsistent` terminal result.

Findings from an agent-perspective audit of afpay. Each item lists the priority,
the file/symbol to change, the concrete fix, and the rationale. Items are
ordered by priority within each section.

The threat model assumed throughout: a possibly-confused agent calls the daemon
over its HTTP API; the operator runs the daemon and configures the allowlist; the
agent must not be able to (a) drain funds beyond configured limits, (b) point a
wallet at an attacker-controlled endpoint, or (c) cause silent double-spends on
retry.

---

> **Status:** 13/15 of the original items landed; item #12's `retry_after_ms`
> half landed in `83302ed` (the closed-enum half is still deferred). After a
> 3-angle post-hoc audit on 2026-05-30 the deferral list shrank: **#8
> `Input::Quote` and #11 wallet-selection ambiguity were re-promoted to P1
> because both have real reliability impact, not just UX.** See those items
> for the concrete agent failure modes.
>
> **Update 2026-08-06 — the plan/confirm boundary landed.** Every operation
> that moves value out of a wallet is now two steps: `send_plan` /
> `cashu_send_plan` resolve the payment and record it, `pay_confirm` executes
> exactly that record. That closed **#8** outright — the plan *is* the quote,
> exposed on the CLI, the pipe, `POST /v1/send-plans`, the confirm window and
> federation — and it moved **#11** from "the agent cannot predict which wallet
> is debited" to "the plan names the wallet before anyone agrees", which is the
> reliability half of that item. See both entries below for what is left.
>
> **Update 2026-08-06 — the gRPC mode is gone.** afpay now has one machine
> face: the HTTP resource API. `RemoteProvider` federates over those same
> `/v1` routes with a Bearer token, and `Mode::Rpc`, `src/mode/rpc/`, `proto/`,
> and the tonic/prost/aes-gcm/hkdf/sha2 dependencies were deleted. Items whose
> subject was the RPC transport are marked **resolved by removal** below —
> the code they describe no longer exists, so there is nothing left to fix and
> nothing left to regress. Items about the *domain* (limits, idempotency,
> allowlists) are untouched: they were never transport-specific.
>
> **Post-hoc audit findings (2026-05-30):** the original list missed several
> load-bearing items, and two landed changes need re-shaping rather than
> straight revert. New items #16–#23 capture the gaps; the "Scope creep"
> section below is now narrowed to S2 (SOL cluster) plus a meta note about
> doc structure (S4). The earlier "revert the RPC handshake" recommendation
> was withdrawn — the handshake's per-session salt actually buys forward
> secrecy against PSK leak (an in-scope threat) and the old 8192-entry FIFO
> replay cache had a real load-cycling bug that the new TTL cache fixes.
> The actual gap is unrelated (item #17). Backward compatibility is **not**
> a constraint when rolling things back — there are no pinned consumers.

---

## Scope creep — to roll back or downgrade

### S1. ~~Revert per-session RPC handshake~~ — **resolved by removal**
- **Status (2026-08-06):** moot. The handshake, the per-session salt, the
  replay cache, and the PSK it protected were deleted with the gRPC mode.
  Federation is HTTP + Bearer over an operator-provided encrypted path
  (Tailscale/WireGuard, an SSH tunnel, or a TLS reverse proxy), so there is no
  afpay-owned cipher left to reason about — and the PSK-leak threat this item
  was defending against no longer has a PSK. The historical reasoning is kept
  below because it explains why the handshake was *not* reverted while it
  existed.
- **Status (2026-05-30):** the earlier recommendation to revert commit `58b16a9` was
  wrong. Three independent reasons surfaced during the 2026-05-30 audit:
  1. **PSK leak is in-scope.** Shell history / docker logs / config
     backups are real exfil paths the operator can't always prevent. With
     a fixed HKDF salt, one PSK leak + any captured pcap = permanently
     decryptable. Per-session salt is forward secrecy on a session axis
     and the cost is one round trip + a small table.
  2. **The old 8192-entry FIFO replay cache was actually broken under
     load.** At the default `requests_per_second = 20` it cycles in ~7
     min; burstier traffic cycles faster, opening a real replay window.
     Commit `58b16a9`'s own message flagged this. The TTL-based cache is
     the right shape.
  3. **The earlier "rainbow table" gloss was reductive.** The commit
     actually does two things — per-session salt isolation *and* replay
     cache rewrite. Both are load-bearing.
- **Action:** none. Superseded by the removal above; item **#17** (handshake
  flood) is likewise resolved by removal.

### S2. SOL cluster pinning via RPC hostname heuristic
- **Where:** commit `dc85f2b`, SOL half — `src/handler/pay.rs` (cluster
  check before send), `src/handler/helpers.rs` (hostname → cluster
  inference), `src/handler/wallet.rs` (`sol_cluster` set at create),
  `src/types/domain.rs` (`sol_cluster: Option<String>`),
  `src/types/protocol.rs` (`--sol-cluster` arg), `src/provider/sol.rs`
  (cluster lookup at send), several test files.
- **Why it's scope creep:** the original spec (`#4`) called for
  `getGenesisHash` — a real on-cluster probe. The implementation
  downgraded to "infer cluster from RPC endpoint hostname", which yields
  **no opinion** on any private / proxied / self-hosted RPC. That's a
  best-effort signal masquerading as a hard refusal (`wrong_cluster`),
  giving readers of the code false confidence.
- **Action — pick one:**
  - **(a) Downgrade to a warning.** Keep the `sol_cluster` metadata field
    and the hostname inference, but on mismatch surface the mismatch on a
    channel the agent **cannot suppress** — either a new `Output::Warning`
    variant or as a structured field on `Output::Sent.trace`. `Output::Log`
    is filterable via `config.log`, so an agent that disables logs sees
    nothing. Document explicitly that the detection is best-effort.
  - **(b) Upgrade to `getGenesisHash`.** Call the active RPC at send time
    (or once at wallet create, cached) and compare the returned hash to
    the well-known mainnet-beta / devnet / testnet genesis hashes. This
    is what the audit asked for; failure to identify the cluster yields
    "no opinion" honestly.
- **Recommendation:** (a). The check is defense-in-depth — a hard refuse
  driven by a heuristic is the wrong shape. EVM `chain_id` pinning in the
  same commit stays as-is for the supplied-value path, but see item #4
  for the still-missing warning when `chain_id` is omitted.

### S3 (watch-list, no action yet). Duplicated schema work
- **Where:** commit `6447abb` added a REST-only `/v1/schema` endpoint
  (~198 lines in `src/mode/rest.rs`); commit `bb7cf13` then introduced
  `Input::Schema` in every mode and the REST handler was rewritten to
  delegate to it (`rest.rs` lost 90 lines, `handler/schema.rs` gained
  99). Net result is fine — the duplication was an evolution path, not
  surviving duplication — but it's a reminder to prefer the cross-mode
  shape first when a discoverability surface is being added.
- **Action:** none. Logged for future reference.

### S4 (meta). Restructure doc for agent-author audience
- **Where:** this file.
- **Problem:** the doc is currently shaped for the auditor (P0/P1/P2/P3
  with action items). An agent author trying to answer "what does afpay
  protect me from today, and what are the sharp edges?" has to read all
  313+ lines and infer the contract.
- **Action:** add a **Guarantees** section at the top (idempotent retries,
  fail-closed allowlist under `--public-listen`, per-network reservation
  TTLs, EVM `chain_id` pin on supplied value, `Input::Schema` in every
  mode, …) and a **Known sharp edges** section beneath it (SOL cluster is
  best-effort heuristic, EVM `chain_id` silently skipped when omitted,
  multi-wallet auto-selection is non-deterministic until #11 lands, no
  fee quote until #8 lands, no `ListReservations` until #20 lands, …).
  The audit backlog (P0/P1/P2/P3) follows. No code changes.

## P0 — Security correctness

### 1. Tighten URL allowlist matching (no host-prefix bypass)
- **Where:** `src/types/config.rs:88` (`url_allowed`)
- **Problem:** Current rule is `url == allowed || url.starts_with(allowed)`. An
  allowlist entry of `https://mint.example` matches
  `https://mint.example.attacker.com/...` because of the bare prefix check.
- **Fix:**
  - Parse both sides as URLs; compare `scheme + host + port` exactly.
  - For path scoping, only treat the allowlist entry as a path prefix when it
    ends in `/`, and require the candidate's path to start with that prefix.
  - Lowercase scheme and host before compare; reject entries with credentials,
    fragments, or query strings.
- **Tests:** add host-prefix bypass case
  (`allow=["https://mint.example"]` rejects `https://mint.example.evil/...`),
  scheme-case (`HTTPS://...`), and `localhost` vs `127.0.0.1` non-equivalence.

### 2. Add `idempotency_key` to `Send` / `CashuSend` ✅ done
- **Where:** `src/types/protocol.rs:13-14` (the existing `// future:
  idempotency_key` comment), `Input::Send` / `Input::CashuSend`,
  `src/handler/pay.rs:288,461`, store layer.
- **Problem:** If the agent times out between (a) tx broadcast and (c)
  reservation confirm (`src/handler/spend_guard.rs:102-134`), it cannot tell
  whether the payment went out. A naive retry double-spends.
- **Fix:**
  - Accept `idempotency_key: Option<String>` (≤128 chars, opaque) on `Send`
    and `CashuSend`.
  - Persist `(key, input_hash, first_output)` in a new
    `idempotency_records` table (redb + postgres). TTL 24 h.
  - On second call with same key:
    - matching `input_hash` → replay first output verbatim;
    - non-matching → return an AFDATA error with `error.code: "idempotency_conflict"`.
  - Reservation lifecycle stays the same; idempotency record stores the
    `reservation_id` so confirm/cancel is also replayable.
- **Tests:** retry-after-broadcast race; mismatched-body conflict; expiry.

### 3. Enforce URL allowlist on `WalletConfigSet`
- **Where:** `src/handler/wallet.rs:619-643` (the `WalletConfigSet` branch).
- **Problem:** `wallet_create` validates `sol_rpc_endpoints` /
  `evm_rpc_endpoints` against the allowlist
  (`src/handler/wallet.rs:54-100`), but `WalletConfigSet` writes them
  unchecked. The op is `is_local_only`, so remote agents can't reach it, but a
  pipe/CLI-mode agent can swap the endpoint to a malicious node.
- **Fix:** factor the existing allowlist check from `wallet_create` into a
  helper (`fn validate_rpc_endpoints(&Config, Network, &[String]) -> Result<…>`)
  and reuse it in `WalletConfigSet` before writing `meta.*_rpc_endpoints`.

### 16. Extend allowlist enforcement to BTC core/electrum and LN endpoints ✅ done
- **Where:** `src/handler/wallet.rs:55-117` (`wallet_create`), `:893`
  (`LnWalletCreate`), `:619-643` (`WalletConfigSet`).
- **Problem:** items #1 and #3 closed the allowlist gap for `mint_url`,
  `btc_esplora_url`, and `*_rpc_endpoints`, but three sibling URL inputs
  were left unchecked, defeating the spirit of both items:
  - `btc_core_url` (Bitcoin Core RPC) — accepted on `wallet_create` and
    `wallet_config_set`, no allowlist check. Agent can point a BTC wallet
    at an attacker-controlled `bitcoind`.
  - `btc_electrum_url` — same as above; agent-controlled Electrum server
    can lie about UTXOs and broadcast txs to anywhere.
  - `LnWalletCreate.request.endpoint` (`wallet.rs:893`) flows into the
    wallet's `mint_url` field with no allowlist check.
- **Fix:** add `allowed_btc_core_urls`, `allowed_btc_electrum_urls`, and
  `allowed_ln_endpoints` to `RuntimeConfig`; reuse the existing
  `validate_url_in_allowlist` helper from item #3. Mirror the change in
  `WalletConfigSet`. Include the new lists in `AllowlistPolicy::banner`
  and `require_for_public_listen`.
- **What landed:** the three new config fields are in place; checks fire
  from `wallet_create` and `LnWalletCreate`; `AllowlistPolicy` reports
  the new counts in `banner()` and `any_set()`; `--public-listen`
  fail-closed text was updated to enumerate the new keys.
- **Still open (small):** add dedicated `validate_url_in_allowlist`
  positive/negative tests for `btc_core_url`, `btc_electrum_url`, and the
  LN endpoint (the helper itself is already covered; this would just
  pin the wiring). `WalletConfigSet` doesn't accept these fields today
  so there's no parallel mutation path to harden.

### 17. Rate-limit the RPC `Handshake` call (session-table flood) ✅ resolved by removal
- **Resolved 2026-08-06:** there is no handshake and no session table. The gRPC
  mode was deleted; the HTTP face has no per-connection state to flood, and its
  own `RateLimiter` covers every route. The remaining sub-items below (separate
  handshake quota, `MAX_SESSIONS` retune, soak test) describe code that no
  longer exists and are closed with it.
- **Where (historical):** `src/mode/rpc/mod.rs:339-354` (`open_session` /
  `Handshake` RPC entry).
- **Problem:** Replaces the "revert handshake" recommendation that was
  withdrawn (see scope creep S1). The handshake call is gated only by the
  global `MAX_SESSIONS = 1024` cap, but is **not** behind
  `RpcRateLimiter`. An unauthenticated peer who can reach the RPC port can
  open 1024 sessions in a tight loop and starve legitimate clients —
  every new session takes a slot and survives `SESSION_IDLE_TIMEOUT = 1h`.
  This is a pre-existing gap in `58b16a9` that the original audit missed.
- **Fix:**
  - Apply `RpcRateLimiter` to the `Handshake` method, same shape as
    `Call`. Use a tighter quota than `Call` (e.g. 2 rps with a small
    burst) — legitimate clients hold a session for a long time, so the
    handshake rate is naturally low.
  - On rate-limit reject, return an AFDATA error with `error.code: "rate_limited"` and
    `retry_after_ms` so clients pace cleanly.
  - Tighten `MAX_SESSIONS` from 1024 to a value that bounds memory under
    burst before rate-limit catches up (e.g. 256 with eviction-on-pressure).
- **What landed:** `Handshake` now goes through the same `try_acquire`
  guard as `Call`. A burst handshake flood is bounded by the operator's
  `rate_limit.requests_per_second` and surfaces as `resource_exhausted`
  (matching `Call`'s existing error shape).
- **Still open:**
  - Separate handshake quota (tighter than `Call`, since legitimate
    handshake rate is much lower).
  - `MAX_SESSIONS` retune + eviction-on-pressure rather than purely
    idle-timeout.
  - Soak test that asserts table size stays bounded under sustained flood.

---

## P1 — Defensive hardening

### 4. EVM `chain_id` check on send; SOL cluster tagging ⚠️ partial (SOL pending S2)
- **Where:** `src/provider/evm.rs:159-162`, `src/provider/sol.rs:177-178`,
  wallet metadata in `src/types/domain.rs`, cluster check at
  `src/handler/pay.rs:728-799`.
- **Problem:**
  - EVM: destination is parsed for checksum but never compared to the wallet's
    `chain_id`. Sending a Base address from an Arbitrum wallet succeeds.
  - SOL: `Pubkey::from_str` accepts a valid base58 key from any cluster;
    mainnet/devnet/testnet are indistinguishable.
- **Fix:**
  - EVM: if the agent supplies `--chain-id` in the request, require it match
    the wallet's `chain_id`; otherwise emit an info-level warning in `trace`.
  - SOL: tag wallet metadata with `cluster: "mainnet-beta"|"devnet"|"testnet"`
    at create time and reject sends if the wallet's RPC endpoint is on a
    different cluster (best-effort detection via `getGenesisHash`).
- **What actually landed (`dc85f2b` + follow-up):**
  - ✅ EVM supplied-value branch refuses on mismatch with `wrong_chain`.
  - ✅ EVM omitted-`chain_id` warning landed — `pay.rs` now emits an
    `evm_chain_unpinned` log when an EVM send proceeds without an
    explicit `chain_id`. Agents that opt in to the `wallet` log filter
    get the visibility; the send is **not** refused (so the happy path
    stays open for callers that don't track chain locally).
  - ⚠️ SOL landed as a hostname heuristic with a hard `Forbidden{wrong_cluster}`
    refuse — still tracked in scope-creep S2 above (downgrade to
    non-suppressible warning, or upgrade to `getGenesisHash`).

### 5. `--public-listen` flips allowlist to fail-closed + banner
- **Where:** `src/types/config.rs:88` (or call site in
  `src/handler/wallet.rs:54-100`), `src/mode/rest.rs` startup banner.
- **Problem:** Empty allowlist = "allow all" is acceptable for laptop use but
  dangerous when the daemon is exposed. Today a publicly-listening daemon with
  an empty list accepts any mint / esplora URL.
- **Fix:**
  - When `--public-listen` is set and `allowed_mint_urls`/`allowed_esplora_urls`
    is empty, refuse to start with a clear error.
  - On startup, print a one-line summary of the active policy:
    `allowlist: mints=N esplora=M (fail-closed)`.

### 6. Reservation TTL per-network, plus reconcile API ✅ done
- **Where:** `src/spend/mod.rs:577` (reservation `expires_at_epoch_ms`),
  `src/handler/spend_guard.rs:149-175` (`AccountingInconsistent`).
- **Problem:** Fixed 5-min TTL is too short for BTC (10+ min confirms can
  outlive the reservation, and the limit "looks like" it has recovered while
  the tx is still in flight) and too long for LN/Cashu (failure detection is
  fast).
- **Fix:**
  - Per-network TTL: Cashu 60 s, LN 90 s, SOL 120 s, EVM 180 s, BTC 30 min.
  - On confirm failure, extend TTL rather than letting the reservation expire.
  - Add `Input::ReconcileReservation { reservation_id, action: confirm|cancel,
    reason: String }` (local-only) so the operator/agent can repair state when
    `AccountingInconsistent` fires.
  - Include `reservation_id` in every `Output::Sent` / `Output::CashuSent` so
    the agent can drive reconciliation.

### 7. Unify schema discovery across all modes
- **Where:** `src/mode/rest.rs:305-392` (existing `/v1/schema`); needed a peer
  for pipe.
- **Problem:** REST had `/v1/schema`; pipe agents had to read the source
  to learn the input field set, error codes, and which inputs are
  local-only.
- **Fix:**
  - Add `Input::Schema` → `Output::Schema { inputs: [...], outputs: [...], error_codes: [...] }` metadata. Available in every mode.
  - In each input descriptor, mark fields `required: bool`, `default: <json>`,
    `secret: bool`, `notes: String`, and `is_local_only: bool` on the input
    itself.
  - Have the REST `/v1/schema` reuse this same builder so the two never drift.

### 8. `Input::Quote` for pre-send fee estimation ✅ done (as `send_plan`)
- **Where:** new input variant; backends already have estimate APIs (EVM
  `eth_gasPrice`, BTC fee estimation, LN melt-quote, Cashu melt-quote).
- **Problem:** `dry_run: true` validates a send but does not return the fee.
  Agents have no canonical way to ask "how much will this actually cost?"
  before committing budget.
- **Concrete agent failure mode (why this is not just UX):** without a
  quote, the agent's only way to know the fee is to hardcode an estimate
  or actually send. Between agent-side budget check and daemon broadcast,
  L1 gas can spike (or LN routing fee can jump). The daemon accepts the
  send because the operator-side spend limit hasn't changed; the
  agent-side accounting overshoots. The mismatch lands in the
  `AccountingInconsistent` path that item #6's reconcile API exists to
  mop up — i.e. shipping #8 reduces the rate at which #6 has to be used.
- **Fix:**
  - `Input::Quote { network, wallet?, to, amount, token? }` →
    `Output::Quote { fee, total_debit, ttl_s, expires_at_epoch_ms }`.
  - Quote does NOT reserve against spend limits (it's read-only).
  - Document quote TTL semantics so agents know not to plan against a stale
    quote.
- **What landed (2026-08-06):** the quote is `Input::SendPlan` /
  `Input::CashuSendPlan` → `Output::PayPlanned { plan_id, wallet, to,
  amount_native, fee_estimate_native, fee_unit, spend_debits,
  expires_at_epoch_ms }`. It is the same shape the item asked for plus the
  thing that makes it load-bearing: the id it returns is what `pay_confirm`
  submits, so a quote is not advice an agent may ignore, it is the only way to
  pay. Resolving reserves nothing, `expires_at_epoch_ms` is the TTL the item
  asked to be documented, and the daemon enforces it rather than trusting the
  caller to notice.

### 18. Close the idempotency crash window between broadcast and finalize
- **Where:** `src/spend/mod.rs:392` (`idempotency_claim`), `:427`
  (`idempotency_finalize`), `src/handler/pay.rs:888-901` (call sites).
- **Problem:** the two-phase claim/finalize is *almost* crash-safe but has
  a narrow window. `idempotency_claim` writes `Pending` before broadcast
  (good); `idempotency_finalize` writes `Final` after broadcast (also
  good). A crash between broadcast and finalize leaves a `Pending` record;
  retry within the 24 h TTL sees `InProgress` and is correctly blocked.
  **But** after the 24 h TTL sweep (`spend/mod.rs:950`), the `Pending`
  row is cleared and the same key returns `Fresh` — a long-delayed retry
  can re-broadcast. The window is narrow but not zero, and the failure
  mode is a double-spend.
- **Fix — pick one:**
  - **(a) Don't sweep `Pending` rows.** Only sweep `Final` rows whose
    write timestamp is older than TTL. `Pending` rows live until an
    operator explicitly clears them (via a new `Input::IdempotencyClear`
    that pairs with reconcile). Trade-off: orphan rows from genuine
    crashes accumulate.
  - **(b) Sweep `Pending` rows but write a tombstone.** Replace expired
    `Pending` with a `Tombstoned` record that keeps returning `InProgress`
    for any new request with the same key. Tombstones are sweepable after
    a much longer horizon (e.g. 30 d).
- **Recommendation:** (b). Operator burden is lower and the failure mode
  closes cleanly.
- **Tests:** simulated crash between phases + retry after TTL expiry;
  assert no double-spend across both (a)/(b) variants.

---

## P2 — Convenience and observability

### 9. Pipe mode: in-flight cap and explicit cancel
- **Where:** `src/mode/pipe.rs:96-112`.
- **Problem:** Pipe spawns concurrent tasks without bound and has no cancel
  path; a runaway agent can fill `in_flight` and starve shutdown
  (`pipe.rs:114-133` already has a 5 s drain timeout, but no proactive cancel).
- **Fix:**
  - Configurable in-flight cap (default 32); excess requests return
    `error.code: "busy"` with `retry_after_ms`.
  - `Input::Cancel { request_id }` cancels the tokio task and runs the
    reservation `cancel()` path if a spend was reserved.
  - Same cancel verb on the HTTP face for parity.

### 10. History ↔ spend reservation cross-link
- **Where:** `HistoryRecord` and `SpendReservation` schemas in
  `src/types/domain.rs` and `src/spend/mod.rs`.
- **Problem:** `history list` returns `transaction_id`, but there is no way to
  trace which spend-limit rule a payment counted against, or which payment a
  reservation belongs to.
- **Fix:**
  - Persist `reservation_id` in `HistoryRecord`.
  - Persist `transaction_id` on the `SpendReservation` after confirm.
  - Expose both in `history status` / `limit list` outputs.

### 11. Disambiguate wallet auto-selection ⚠️ partial (the reliability half is closed)
- **Where:** mint-URL-based wallet selection in `cashu` send path
  (`src/provider/cashu.rs`); analogous EVM/SOL multi-wallet selection in
  `src/handler/pay.rs`.
- **Problem:** "Picks first wallet with sufficient balance" is documented in
  the README but invisible to the agent — it cannot predict which wallet will
  be debited. The agent's spend-limit accounting may not match the daemon's.
- **Concrete agent failure mode (why this is not just UX):** the agent
  thinks "wallet X has $1000, I can spend $500" and counts the spend
  against wallet X locally. The daemon picks wallet Y (also has
  sufficient balance) and debits it. Next call assumes wallet X
  untouched, hits an unexpected `limit_exceeded` on wallet Y, or worse,
  silently keeps spending from a wallet the agent thought was reserved.
  This compounds with item #4 SOL cluster pinning: the daemon may pick a
  wallet whose `sol_cluster` happens to match the active RPC endpoint,
  so the cluster check passes — and the agent's intent (which cluster to
  send on) is silently lost.
- **Fix:**
  - If `--wallet` not given and >1 wallet matches the filter, return
    `Output::Ambiguous { candidates: [{wallet_id, label, balance}, ...] }`
    and require the agent to pick one.
  - Opt-in `--auto-select first-with-balance` for callers that genuinely
    want the old behaviour.
- **What landed (2026-08-06):** the failure mode above is gone. Selection now
  happens while resolving a plan, and `Output::PayPlanned.wallet` names the
  wallet the provider picked *before* anything is confirmed — so the agent's
  accounting can follow the daemon's rather than guess at it, and a plan whose
  wallet metadata moves afterwards is refused with `plan_stale` rather than
  paid. The wallet is also pinned into the plan record, so the confirm cannot
  land on a different one than the plan named.
- **Still open:** the *choice* is still afpay's. When several wallets match,
  the plan reports the pick instead of returning `Output::Ambiguous` and
  making the caller choose. That is now a UX gap rather than a correctness
  one, so it stays open at P2 weight.

### 12. `error.code` becomes a closed enum ⚠️ partial
- **Where:** `src/types/protocol.rs` `Output::Error`, all `emit_error` call
  sites.
- **Problem:** Some `error.code` values are constructed ad-hoc as
  `internal_error("…")`. Agents can't pattern-match reliably.
- **Fix:**
  - Define `pub enum ErrorCode { … }` with `Display`/`Serialize` to
    snake_case strings; require all `PayError` constructors to map to a
    variant.
  - The HTTP contract and `Output::Schema` enumerate the closed set.
  - Add `retry_after_ms: Option<u64>` to `Output::Error` for `busy`,
    `rate_limited`, `temporary_network_error`.
- **What landed (`83302ed`):** `retry_after_ms` half is done. The
  closed-enum half is still open. Lower priority — agents can string-match
  today and `Output::Schema` already enumerates the live set; revisit
  only if an agent author actually trips on it.

---

## P3 — Smaller correctness items

### 13. EVM `u256 → u64` becomes checked
- **Where:** `src/provider/evm.rs:243` (`u256_to_u64_saturating`).
- **Fix:** replace saturation with `u64::try_from` → `PayError::InvalidAmount`
  on overflow. Matches the spirit of commit `b16eab7` (checked fee math).

### 14. Audit container entrypoint for secret generation
- **Where:** `container/docker/` entrypoint scripts (not covered by this
  audit).
- **Verify:**
  - `AFPAY_REST_API_KEY_SECRET` generated from a CSPRNG
    (`openssl rand -base64 32` or `/dev/urandom`), never `$RANDOM` /
    `date | md5`.
  - Persisted files are `chmod 600`, in the data volume only.
  - Secret values are never echoed to stdout/stderr or `docker logs`; only
    paths and endpoints are printed.

### 15. Pipe parse-error verbosity
- **Where:** `src/mode/pipe.rs:78-88`.
- **Problem:** Raw `serde_json` errors leak field names and positions; useful
  for development, mildly informative to attackers enumerating the schema.
- **Fix (optional):** when `--public-listen` is set or pipe is exposed over
  a socket, replace the raw serde message with a generic
  `"parse error at byte N"` and emit the detail only to the daemon log.

---

## P2 — New items from 2026-05-30 audit (agent-author UX gaps)

### 19. Lazy-expire reservations on read paths
- **Where:** `src/spend/mod.rs:2053-2085` (`pg_expire_pending`),
  `src/handler/spend_guard.rs`.
- **Problem:** expired-but-not-yet-swept reservations can be returned by
  read paths between sweeps. Sweep only fires inside other writes, so a
  quiescent system can hold a "still pending" reservation past its TTL
  until the next write happens. Agents that poll `limit list` between
  sends see stale counts.
- **Fix:** on every reservation read, expire-on-read by comparing
  `expires_at_epoch_ms` to `now`; downgrade an expired `Pending` to
  `Expired` before returning. Keep the bulk sweep as a write-path
  amortisation, but don't rely on it for correctness of reads.
- **Tests:** read `limit list` after TTL elapses without an intervening
  write; expect the reservation to be reported `Expired`, not `Pending`.

### 20. `Input::ListReservations` for agent recovery
- **Where:** new input variant, paired with #6's reconcile API.
- **Problem:** after an agent crash and restart, the agent has a
  `reservation_id` it may or may not have persisted, but no way to
  enumerate stuck reservations to drive reconcile. The operator can
  `psql`; the agent can't.
- **Fix:** `Input::ListReservations { wallet?, status?: pending|expired|all }`
  → `Output::Reservations { items: [{ reservation_id, wallet_id, amount,
  status, created_at_epoch_ms, expires_at_epoch_ms }, ...] }`. Pairs with
  `Input::ReconcileReservation` (#6) to give the agent a full
  recovery loop without operator intervention.

### 21. Structured fields on `Output::*.trace`
- **Where:** `trace` field on every `Output::*` variant
  (`src/types/protocol.rs`).
- **Problem:** `trace` is free-form. Agents that want to slice telemetry
  by event/wallet/request have to regex log strings, which breaks on every
  message wording tweak.
- **Fix:** make `trace` a structured object: `{ event: String, wallet_id?:
  String, request_id?: String, latency_ms: u64, ...kvs }`. Keep
  human-readable rendering as a `Display` impl on the struct so existing
  CLI output is preserved. `Output::Schema` enumerates the keys.

### 22. `schema_version` on `Output::Schema` ⚠️ partial
- **Where:** `src/handler/schema.rs`.
- **Problem:** agents discovering the schema have no version anchor —
  they can't safely cache the result or compare against a known-good
  shape. Any silent input-field addition (e.g. #18's
  `IdempotencyClear`) is invisible until the agent re-fetches and diffs.
- **Fix:** add `schema_version: String` (monotonic, e.g. `"2026-05-30.1"`)
  and `git_sha: String` to `Output::Schema`. Bump `schema_version` in
  every commit that touches `Input::*` / `Output::*` / `ErrorCode`.
- **What landed:** `wire_protocol_schema()` now emits
  `"schema_version": "2026-05-30.1"`. Bump it on every shape change.
- **Still open:** `git_sha` (requires a build-time embed via env / vergen
  in `build.rs`) — leave until an agent author actually asks for it.

### 23. Document partial-failure semantics on `Output::Sent`
- **Where:** docs + handler comments in `src/handler/pay.rs`.
- **Problem:** the agent currently can't tell from `Output::Sent`
  whether the daemon means (a) "broadcast acknowledged by mempool /
  network" or (b) "reservation confirmed against the spend ledger". The
  two diverge under `AccountingInconsistent`; an agent that retries on
  any non-`Sent` output may double-spend; an agent that doesn't retry
  may leave a stuck reservation.
- **Fix:** add an `Output::Sent.commit_status: enum { Broadcast,
  Confirmed, Reconciling }` field. Document in the skill file that
  `Broadcast` means "you have a `reservation_id`; check `ReconcileReservation`
  or `ListReservations` (#20) before retrying"; `Confirmed` means "safe to
  retry-free"; `Reconciling` means "operator action needed". No protocol
  change is needed for `Output::CashuSent` (instant).

---

## Summary by priority

Legend: ✅ done · ⚠️ partial · ❌ open · 🔺 promoted on 2026-05-30 audit

| P  | # | Item                                                | Status | Touches                                |
|----|---|-----------------------------------------------------|--------|----------------------------------------|
| P0 | 1 | URL allowlist exact origin match                    | ✅     | `types/config.rs`                       |
| P0 | 2 | `idempotency_key` for sends                         | ✅     | `protocol.rs`, store, `handler/pay`     |
| P0 | 3 | Allowlist on `WalletConfigSet`                      | ✅     | `handler/wallet.rs`                     |
| P0 | 16| Allowlist BTC core/electrum + LN endpoint           | ✅     | `handler/wallet.rs`, `types/config.rs`  |
| P0 | 17| Rate-limit RPC `Handshake` (session-table flood)    | ✅     | resolved by removal — gRPC mode deleted  |
| P1 | 4 | EVM chain-id check; SOL cluster tag                 | ⚠️     | EVM done; SOL via S2; `handler/pay`     |
| P1 | 5 | `--public-listen` ⇒ fail-closed + banner            | ✅     | `types/config`, mode startup            |
| P1 | 6 | Per-network reservation TTL + reconcile API         | ✅     | `spend/mod`, `handler/spend_guard`      |
| P1 | 7 | `Input::Schema` in all modes                        | ✅     | `protocol.rs`, all modes                |
| P1 | 8 | `Input::Quote` for fee estimation                   | ✅     | shipped as `send_plan` / `pay_planned`  |
| P1 | 11| Multi-wallet selection ambiguity output             | ⚠️     | plan names the pick; no `Ambiguous` yet |
| P1 | 18| Close idempotency crash window (Pending tombstone)  | ❌     | `spend/mod.rs`, `handler/pay.rs`        |
| P2 | 9 | Pipe in-flight cap + `Input::Cancel`                | ⚠️     | `mode/pipe.rs` (cap done; Cancel open)  |
| P2 | 10| History ↔ reservation cross-link                    | ✅     | `types/domain`, `spend/mod`             |
| P2 | 12| Closed `ErrorCode` enum + `retry_after_ms`          | ⚠️     | `protocol.rs`, all `emit_error`         |
| P2 | 19| Lazy-expire reservations on read paths              | ❌     | `spend/mod.rs`, `handler/spend_guard`   |
| P2 | 20| `Input::ListReservations` for agent recovery        | ❌     | `protocol.rs`, `spend/mod`              |
| P2 | 21| Structured `trace` fields on every `Output::*`      | ❌     | `types/protocol.rs`                     |
| P2 | 22| `schema_version` on `Output::Schema`                | ⚠️     | `handler/schema.rs` (git_sha open)      |
| P2 | 23| Document partial-failure semantics on `Output::Sent`| ❌     | docs + `handler/pay.rs`                 |
| P3 | 13| EVM `u256 → u64` checked                            | ✅     | `provider/evm.rs`                       |
| P3 | 14| Container secret-generation audit                   | ✅     | `container/docker/`                     |
| P3 | 15| Pipe parse-error scrubbing under public-listen      | ✅     | `mode/pipe.rs`                          |

Scope-creep section (above): S1 resolved by removal (gRPC mode deleted) · S2 still open · S3 logged · S4 (doc restructure) open.

---

## 2026-08-06 re-check of every remaining item

The plan/confirm work touched the pay path, the REST face and federation, so
every open item was re-read against the current tree rather than carried
forward on trust. What follows is what is actually true today.

**Closed by this batch.** #8 (the plan is the quote, and it is mandatory).

**Improved but not closed.** #11 — the plan names the wallet it picked, which
removes the accounting divergence the item was promoted for. Choosing is still
afpay's.

**Verified still open, unchanged by this batch.** S2 (the SOL cluster check is
still a hostname heuristic behind a hard `wrong_cluster` refuse; it moved from
the send into `check_chain_pins` at plan time, which changes when it fires, not
what it knows), #12's closed-enum half (`PayError` gained `plan_not_found`,
`plan_expired` and `plan_stale` as ordinary variants; the codes are still
`&'static str` at the edge), #18 (`Pending` idempotency rows are still swept at
the 24h TTL with no tombstone — note the plan's own single-use claim now bounds
the same window for payments, because a swept key cannot resurrect a plan that
was already consumed), #19 (reservations are still expired by the write-path
sweep, not on read), #20 (no `ListReservations`), #21 (`trace` is still
`{duration_ms}`), #23 (`Output::Sent` still has no `commit_status`).

**Re-checked and closed as already done.** #9's cap half — `mode/pipe.rs`
enforces `PIPE_IN_FLIGHT_CAP` and answers `busy` with `retry_after_ms`;
`Input::Cancel` is still absent. #22's `git_sha` half — `build.rs` embeds
`GIT_SHA` and `--version` reports it, so the remaining gap is only that
`Output::Schema` does not repeat it.

**Newly relevant.** §8 of the Provider OpenAPI baseline required a persistent
`Idempotency-Key` on every local mutation a retry could duplicate. `wallet
create` and `receive` had none because the replay store was a closed enum over
three payment outcomes. That store now carries `WalletCreated`, `ReceiveInfo`
and `ReceiveClaimed` as well, both routes require the header, and
`src/handler/idempotency.rs` is the single implementation the CLI and the HTTP
face share. A replayed `wallet create` deliberately does not re-emit a
generated mnemonic: a 24-hour replay record is not a place to keep key
material.
