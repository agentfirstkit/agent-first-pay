# Container Layout

- `docker/` holds the canonical image build (one merged `Dockerfile` with a
  `builder` source stage and a `downloader` source stage, selected by the
  `AFPAY_BIN_FROM` build-arg), the compose stack, and the supervisord config.
- `apple-container/` holds only local runtime state now (gitignored `data/` +
  `backups/`); the launcher scripts were retired in favour of `afpay container`.

## Running it

`afpay container install` is the one-command path: it builds the image from the
recipe embedded in the binary and runs the daemon, under **Docker, Podman, or
Apple `container`** (auto-detected; override with `--runtime`). By default it
selects the Dockerfile's `downloader` stage, which downloads the matching
prebuilt release — no source tree, no Rust toolchain in the image. Use
`afpay container status` to reprint the endpoint + client command, `logs` to
tail, and `uninstall` (`--purge` also drops the image + cache) to tear down.

```bash
afpay container install --allow mint=https://mint.example   # see allowlist note below
afpay container install --with phoenixd --allow ln=http://127.0.0.1:9740
afpay container install --mode rpc             # RPC (gRPC + PSK) instead of REST
afpay container install --from-source          # compile from this checkout
```

**Operator allowlist (required).** afpay refuses to start a public listener with
an empty allowlist — an agent-unreachable boundary on which external endpoints it
may contact. Pass at least one `--allow <category>=<url>` (categories: `mint`,
`esplora`, `sol-rpc`, `evm-rpc`, `btc-core`, `btc-electrum`, `ln`; repeatable).
These seed the `allowed_*` arrays in `config.toml`, which is written once per data
volume — to change them later, edit `config.toml` in the volume or recreate it.

`--from-source` builds the Dockerfile's `builder` stage from the current
directory (or `--context <dir>`) — used for development or an unreleased version.
Under **Apple `container`**, compiling needs a bigger builder VM than the 2 GiB
default (afpay pulls heavy crates); resize it once with
`container builder stop && container builder delete && container builder start --cpus 4 --memory 8g`.
The download path never compiles, so it is unaffected.

From a source checkout you can also drive the runtime CLI directly:

```bash
docker compose -f container/docker/compose.yaml up --build   # or: podman compose …
podman build -t afpay -f container/docker/Dockerfile .
```

The entrypoint stores the REST API key or RPC PSK under the afpay data volume
with private file permissions and passes it via environment variable. It does not
print secret values or include them in the `afpay` process arguments.

## Backup and Restore

- `container/docker/backup.sh` and `container/docker/restore.sh` back up
  Docker/Podman named volumes. Override `AFPAY_VOLUME`, `PHOENIXD_VOLUME`, and
  `BITCOIND_VOLUME` if your actual volume names are project-prefixed (an
  `afpay container install --name N` host uses `N-afpay` / `N-phoenixd` /
  `N-bitcoind`). You can also snapshot directly:
  `<runtime> exec <name> afpay global backup …`.
- By default, backups include `afpay` and `phoenixd`. Set `INCLUDE_BITCOIND=true`
  when you also want the local `bitcoind` data.
- `bitcoind` is excluded by default because it can resync, while recovery-critical
  wallet state lives in `afpay` and `phoenixd`.
- If `storage_backend = "postgres"`, you must also back up PostgreSQL separately;
  the container scripts only cover mounted `/data/*` state.
