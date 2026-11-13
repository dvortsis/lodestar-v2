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

## Chapter 4: Install on Windows and Linux

1. So the program and the scripts Windows runs as SYSTEM stay out of other users' reach. It always goes in `C:\Program Files\Lodestar`.
2. Your data in `C:\ProgramData\Lodestar`: library, password and keys. To remove it too, run `msiexec /x <the MSI you installed> REMOVE_DATA=1` from an elevated PowerShell, after a backup.
3. To a pool path such as `/mnt/cache/appdata/lodestar`. `/mnt/user` goes through Unraid's FUSE layer, which is slow for the index and risky for the database.
4. Open **Settings → Crawler**, set **Crawl intensity** one step lower, then restart Lodestar with `Restart-Service Lodestar`, `sudo systemctl restart lodestar`, or by starting the portable ZIP again. The change applies at the next start.

## Chapter 7: Connect other apps

1. So you can revoke or regenerate one app's key without breaking the others, and a leaked key reaches only one app.
2. In Sonarr, open the Lodestar indexer and add category `5045` to **Categories**, and `5000` too. Through Prowlarr, Sonarr gets both by itself.
3. Regenerating stops the old key at once, and Prowlarr still has it. Paste the new key into **API Key** in Prowlarr and test again.
4. Delete it and select **Download export** again. A failed export ends partway, and nothing inside the file says so.

## Chapter 8: Add companions

1. Starting a container needs the Docker socket, which gives control of every container on the host, the same as root. Lodestar shows the file and you apply it.
2. No. Never forward a companion's web page port. Reach it over a VPN instead.
3. qBittorrent, because Lodestar sets it up most fully. You confirm each install so that nothing is downloaded, and nothing runs, unless you've seen it and its SHA-256 matches.
4. Choose qBittorrent under **Companion** in **Companions you already run**, type its **Address**, such as `http://192.168.1.20:8080`, and select **Add**.

<!-- nav:start -->
---

[← Chapter 8: Add companions](08-add-companions.md) · [Contents](README.md#contents) · [Appendix B: Settings reference →](appendix-b-settings-reference.md)
<!-- nav:end -->
