# Appendix A: Answers to check your understanding

Answers to the questions at the end of each chapter, in the same order.

## Chapter 1: Before you start

1. No. It fetches only each torrent's description, so no content lands on your disk.
2. The index does random reads and writes, which spinning disks and network shares handle slowly.
3. No. Lodestar works without a router change, and forwarding is optional.

## Chapter 2: Install Lodestar on Unraid and Docker

1. Use `/mnt/cache/appdata/lodestar` or your pool's path. `/mnt/user` goes through Unraid's FUSE layer, which is slow and risky for SQLite.
2. Make the folder writable by the container's user: set `--user 99:100` in **Extra Parameters** and `chown` the folder to `99:100`.
3. Once the admin password exists. After that no token is printed or needed.

## Chapter 3: Keep Lodestar safe

1. Set up a VPN such as WireGuard or Tailscale and connect through it. Don't forward port `8090` or use the router's DMZ.
2. It shows your public IP address to DHT peers.
3. `seal.key`. Without it, stored secrets in the restored database can't be read.

<!-- nav:start -->
---

[← Chapter 6: Troubleshooting](06-troubleshooting.md) · [Contents](README.md#contents) · [Appendix B: Settings reference →](appendix-b-settings-reference.md)
<!-- nav:end -->
