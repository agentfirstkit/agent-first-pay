# Agent-First Pay

A payment tool for AI agents — send and receive across five networks through one interface, with spending limits you control.

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
- **A shared payment daemon** — run afpay on a trusted machine; agents send requests, limits stay enforced server-side.
- **Accepting payments** — generate invoices and watch for incoming funds on any of the five networks.

## Install

```bash
brew install agentfirstkit/tap/afpay   # macOS / Linux
cargo install agent-first-pay          # any platform
```

## Docs

- [Overview](docs/overview.md) — the full guide: every network, setup, and examples
- [CLI Reference](docs/cli.md) — every command and flag
- [Architecture](docs/architecture.md) — how it is built, deployment patterns
- [Testing](docs/testing.md) — unit and integration tests

## License

MIT
