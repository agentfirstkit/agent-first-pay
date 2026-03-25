# Container Layout

- `docker/` holds the canonical image build, compose stack, and supervisord config used by both Docker and Podman.
- `apple-container/` holds the macOS Apple `container` CLI launcher scripts and notes.

Use `docker compose -f container/docker/compose.yaml up --build` for the standard flow, or `./container/apple-container/up.sh` on macOS when you want to run through Apple `container`.

## Backup and Restore

- `container/apple-container/backup.sh` and `container/apple-container/restore.sh` back up the Apple Container CLI bind-mounted data directories.
- `container/docker/backup.sh` and `container/docker/restore.sh` back up Docker/Podman named volumes. Override `AFPAY_VOLUME`, `PHOENIXD_VOLUME`, and `BITCOIND_VOLUME` if your actual volume names are project-prefixed.
- By default, backups include `afpay` and `phoenixd`. Set `INCLUDE_BITCOIND=true` when you also want the local `bitcoind` data.
- `bitcoind` is excluded by default because it can resync, while recovery-critical wallet state lives in `afpay` and `phoenixd`.
- If `storage_backend = "postgres"`, you must also back up PostgreSQL separately; the container scripts only cover mounted `/data/*` state.
