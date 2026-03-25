# Apple Container CLI on macOS

This directory provides the macOS Apple `container` CLI workflow for `afpay`.

It reuses the canonical `container/docker/Dockerfile`, so the image definition stays in one place while macOS users can run it through Apple's open-source `container` runtime instead of Docker Desktop.

## Prerequisites

Install Apple's `container` CLI from the GitHub releases page, then start the system service:

```bash
container system start
```

## Start

```bash
./container/apple-container/up.sh
```

The launcher will:

```bash
container system start
container build --platform linux/arm64 -t afpay:apple -f container/docker/Dockerfile .
container run --name afpay-apple ...
```

## Variants

```bash
AFPAY_MODE=rpc AFPAY_PORT=9400 ./container/apple-container/up.sh
ENABLE_BITCOIND=true ./container/apple-container/up.sh
./container/apple-container/backup.sh
./container/apple-container/restore.sh ./container/apple-container/backups/afpay-apple-backup-YYYYMMDDTHHMMSSZ.tar.gz
./container/apple-container/logs.sh
./container/apple-container/down.sh
```

## Notes

- Apple `container` currently targets Apple silicon Macs and macOS 26+.
- `up.sh` defaults to detached mode.
- `up.sh` builds for `linux/arm64` by default. Override with `AFPAY_APPLE_BUILD_PLATFORM=...` if needed.
- `bitcoind` is disabled by default. Set `ENABLE_BITCOIND=true` to build and start a local `mainnet` node.
- When enabled, `bitcoind` defaults to `BTC_NETWORK=mainnet`, `BTC_RPC_PORT=8332`, and `BTC_PRUNE_MB=550`. Set `BTC_PRUNE_MB=0` to disable pruning.
- If `INSTALL_BITCOIND` is not set, `up.sh` automatically matches it to `ENABLE_BITCOIND`.
- `backup.sh` and `restore.sh` snapshot the local `data/` directories used by Apple `container`. By default they include `afpay` and `phoenixd`; set `INCLUDE_BITCOIND=true` to include the local `bitcoind` data too.
- `afpay`-managed Cashu, BTC, Solana, and EVM wallets store their recovery mnemonics inside `/data/afpay`. `phoenixd` must be backed up from `/data/phoenixd/.phoenix/`.
- The wrapper intentionally sticks to core commands: `system start`, `build`, `run`, `logs`, `stop`, and `delete`.
