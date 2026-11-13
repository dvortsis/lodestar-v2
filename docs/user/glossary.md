# Glossary

Terms used in this guide, A to Z. Each chapter links a term here the first time it uses it.

## bitmagnet

Another self-hosted torrent indexer. Lodestar can export its torrents as a file that bitmagnet imports.

## Cache pool

A group of SSDs or NVMe drives in Unraid, used for fast storage. A path starting `/mnt/cache` is on it.

## Checksum

A short fingerprint of a file. If the file changes, the fingerprint changes, so you can check a download.

## Companion

A separate program that Lodestar works with, such as qBittorrent, Prowlarr or gluetun. Lodestar finds it, configures it and checks it, and never bundles it.

## Container

A packaged, running copy of an app. An update replaces it, so only mapped folders keep their files.

## DHT

Distributed hash table: the way BitTorrent nodes find torrents and each other without a central server. It uses UDP.

## DMZ

A router setting that sends all unrequested internet traffic to one device. Never use it for Lodestar.

## Dynamic port range

The block of temporary local ports Windows hands out to outgoing connections. Lodestar needs enough of them left for its peer connections.

## Elevated PowerShell

A PowerShell window opened with **Run as administrator**. Windows asks you to confirm. Only an elevated window can read Lodestar's private Windows data folder.

## FUSE

The layer behind Unraid's `/mnt/user` paths. It adds overhead that databases notice.

## Indexer

A service that apps such as Sonarr and Radarr search for torrents. Lodestar acts as one through Torznab.

## Install window

A daily time range, in the PC's local time, when automatic updates may install. **Install now** ignores it. Windows installs nothing automatically.

## Metadata

The description a torrent publishes about itself: name, file list, sizes and dates. It is not the content.

## Named volume and bind mount

A named volume is a data folder Docker manages for you. A bind mount is a folder you pick on the host.

## Network or shared-folder filesystem

NFS, SMB, a Windows network drive, or a shared folder such as the ones WSL2 and Docker Desktop mount from a Windows drive. Its file locking isn't reliable enough for Lodestar's database.

## Newznab category

A number that files a result by kind, such as `5000` for TV or `2000` for movies. Apps search only the categories you choose.

## Port forwarding

A router rule that sends traffic from the internet to one device on your network.

## Port mapping

A rule that connects a port on your server to the port an app listens on inside its container.

## Probation

The check Lodestar runs on a new version right after an update. A service must keep running for a minute. A portable copy must also open your library and answer a search. If it fails, the update is rolled back.

## PUID and PGID

The user ID and group ID a container runs as. They decide who owns the files in the data folder.

## Setup token

A one-time key printed in the log on first run. You enter it to create the admin password.

## SmartScreen

The Windows feature that shows "Windows protected your PC" for apps it doesn't recognise.

## SQLite

The database Lodestar stores its torrents in. It needs fast local storage.

## TLS reverse proxy

A web server in front of an app that adds HTTPS. Lodestar needs `LODESTAR_SECURE_COOKIES=true` behind one.

## Torznab

The common way apps such as Prowlarr, Sonarr and Radarr search an indexer for torrents.

## Torznab key

A key that lets one app search Lodestar through Torznab, and nothing else. It is shown once, when you create or regenerate it.

## UDP

A network protocol without a connection setup, used for peer discovery. Firewall and router rules list it separately from TCP.

## Update rollback

Putting the previous version back after an update fails its probation, on its own. It also restores your library from a snapshot taken before the update, if there was one, which drops torrents found since.

## VPN

A private tunnel to another network. It can hide your home IP address from peers and let you reach home safely from outside.

<!-- nav:start -->
---

[← Appendix B: Settings reference](appendix-b-settings-reference.md) · [Contents](README.md#contents) · [Revisions →](revisions.md)
<!-- nav:end -->
