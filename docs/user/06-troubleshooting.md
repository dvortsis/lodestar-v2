# Chapter 6: Troubleshooting

This chapter lists common problems with fixes, then says how to get help.

## 6.1 Common problems

| Problem | Likely cause | Fix |
|---|---|---|
| The web page doesn't load | The [container](glossary.md#container) isn't running, or the port is taken | Check that the container shows as started. On Unraid, open the Docker tab. With Compose, run `docker compose ps` on the Docker host. If another app uses `8090`, change the host side of the **Web UI** port. |
| Lodestar exits with "started as root without the SETUID/SETGID capabilities" | The container was started with those permissions removed | In Compose, remove `cap_drop: ALL`, or add `cap_add: [SETUID, SETGID, CHOWN]` after it ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| Lodestar exits saying the data folder "doesn't look like Lodestar's" | **Data** holds other files and belongs to someone else | Point **Data** at an empty folder, such as `/mnt/cache/appdata/lodestar` ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| **Apply** or `docker compose up` fails with "manifest unknown" | No release is published yet, so `latest` doesn't exist | Wait for the first release, or use the `nightly` tag to try Lodestar. It is untested. |
| You lost the [setup token](glossary.md#setup-token) | The log scrolled, or you haven't created a password yet | Restart the container. Read the log again and use the newest token. |
| Nothing is discovered | The [DHT](glossary.md#dht) can't bootstrap, or outbound [UDP](glossary.md#udp) is blocked | Read the log for DHT errors. Check that DNS and outbound UDP work from the server. Without a forwarded port the crawler is slow but still works. |
| The database is slow or reports "database is locked" | The data folder is under `/mnt/user` or on a network share | Move the data folder to a pool path such as `/mnt/cache/appdata/lodestar` ([section 2.2](02-install-lodestar-on-unraid-and-docker.md#22-settings-in-the-unraid-template)). |
| Your home network gets slow | The crawler fills the router's connection table | Set **Intensity** to `low` and raise it step by step. |
| The gluetun variant stops after a [VPN](glossary.md#vpn) restart | A container that shares a network stays attached to the old one | Restart Lodestar after gluetun restarts. |

## 6.2 Get help

Open an issue at <https://github.com/dvortsis/lodestar-v2/issues>. Include your Lodestar version, install method, what you expected and what happened. Add the last lines of the log with the setup token, IP addresses and torrent names removed.

<!-- nav:start -->
---

[← Chapter 5: Use Lodestar](05-use-lodestar.md) · [Contents](README.md#contents) · [Appendix A: Answers to check your understanding →](appendix-a-answers.md)
<!-- nav:end -->
