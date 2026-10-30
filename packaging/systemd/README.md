# systemd unit

`lodestar.service` runs `lodestar serve` as a transient unprivileged user
(`DynamicUser=`) with its state in `/var/lib/lodestar`.

## Install

```sh
sudo install -m 0755 lodestar /usr/local/bin/lodestar
sudo install -m 0644 lodestar.service /etc/systemd/system/lodestar.service
sudo systemctl daemon-reload
sudo systemctl enable --now lodestar
```

Find the first-run setup token in the log, then open `http://<host>:8090/`:

```sh
journalctl -u lodestar | grep "setup token"
```

## Notes

- **Ports:** `8090/tcp` for the web UI and `6881/udp` for the DHT. Open them in
  your firewall if one is active.
- **Settings:** override with a drop-in (`sudo systemctl edit lodestar`), for
  example `Environment=LODESTAR_INTENSITY=low`. The variables that exist today
  are listed in `docs/operations.md`.
- **Priority:** `Nice=10` and a lower I/O priority keep the crawler from
  competing with the rest of the host. Remove them if this machine is
  dedicated to Lodestar.
- **File limit:** `LimitNOFILE=8192` covers 400 metadata sockets with
  headroom, including the roughly 1000 sockets of
  `LODESTAR_INTENSITY=aggressive`.
- **Data elsewhere:** `ProtectSystem=strict` makes everything outside the
  state directory read-only. To keep data on another disk, bind-mount it to
  `/var/lib/lodestar` or add its path to `ReadWritePaths=` and set
  `LODESTAR_DATA_DIR`. With `DynamicUser=`, a custom path must be writable by
  the dynamic user, so a drop-in with a fixed `User=` is simpler there.
- **Restart record:** with `[dht] safe_restart` on, a clean restart can
  resume sampling at once. Its record lives in `/var/lib/lodestar-witness`,
  a second state directory beside the data directory. Back up
  `/var/lib/lodestar`, never the two together: restoring both would hide
  the restore. With `DynamicUser=` both really live under
  `/var/lib/private/`, so don't back up `/var/lib/private` as a whole.
- **Check the unit:** `systemd-analyze verify lodestar.service` (the binary
  path must exist for the check to pass).
