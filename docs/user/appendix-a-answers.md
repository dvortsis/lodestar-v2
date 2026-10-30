# Appendix A: Answers to check your understanding

Answers to the questions at the end of each chapter, in the same order.

## Chapter 1: Before you start

1. No. It fetches only each torrent's description, so no content lands on your disk.
2. The index does random reads and writes, which spinning disks and network shares handle slowly.
3. No. Lodestar works without a router change, and forwarding is optional.

## Chapter 2: Install on Unraid and Docker

1. Use `/mnt/cache/appdata/lodestar` or your pool's path. `/mnt/user` goes through Unraid's FUSE layer, which is slow and risky for SQLite.
2. Leave **PUID** at `99` and **PGID** at `100`, and never use `0`. Lodestar makes **Data** belong to that user on start, and `0` is root.
3. Once the admin password exists. After that no token is printed or needed.

## Chapter 3: Keep Lodestar safe

1. Set up a VPN such as WireGuard or Tailscale and connect through it. Don't forward port `8090` or use the router's DMZ.
2. It shows your public IP address to DHT peers.
3. The backup holds `seal.key` and `auth.json`, so a public share would expose secrets. A restore also brings back the sessions valid at backup time, which last up to 30 days. Changing the password ends them, once the app offers that **(planned)**.
4. A restore that brings back both looks like a clean restart, so Lodestar would ask DHT nodes again too soon. Back up the data folder with `lodestar backup`, which leaves the record out.

<!-- nav:start -->
---

[← Chapter 6: Troubleshooting](06-troubleshooting.md) · [Contents](README.md#contents) · [Appendix B: Settings reference →](appendix-b-settings-reference.md)
<!-- nav:end -->
