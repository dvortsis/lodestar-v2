# Glossary

Terms used in this guide, A to Z. Each chapter links a term here the first time it uses it.

## Cache pool

A group of SSDs or NVMe drives in Unraid, used for fast storage. A path starting `/mnt/cache` is on it.

## Checksum

A short fingerprint of a file. If the file changes, the fingerprint changes, so you can check a download.

## Container

A packaged, running copy of an app. An update replaces it, so only mapped folders keep their files.

## DHT

Distributed hash table: the way BitTorrent nodes find torrents and each other without a central server. It uses UDP.

## DMZ

A router setting that sends all unrequested internet traffic to one device. Never use it for Lodestar.

## FUSE

The layer behind Unraid's `/mnt/user` paths. It adds overhead that databases notice.

## Metadata

The description a torrent publishes about itself: name, file list, sizes and dates. It is not the content.

## Named volume and bind mount

A named volume is a data folder Docker manages for you. A bind mount is a folder you pick on the host.

## Port forwarding

A router rule that sends traffic from the internet to one device on your network.

## Port mapping

A rule that connects a port on your server to the port an app listens on inside its container.

## PUID and PGID

The user ID and group ID a container runs as. They decide who owns the files in the data folder.

## Restart record

What Lodestar writes when it stops cleanly, so the next start can skip the six-hour wait before sampling the DHT. Part of it lives in a separate restart record folder that is never backed up with the data folder. See [section 3.8](03-keep-lodestar-safe.md#38-restart-without-the-six-hour-wait).

## Safe restart

The setting that lets a clean restart skip the six-hour wait. It is off by default.

## Setup token

A one-time key printed in the log on first run. You enter it to create the admin password.

## SmartScreen

The Windows feature that shows "Windows protected your PC" for apps it doesn't recognise.

## SQLite

The database Lodestar stores its torrents in. It needs fast local storage.

## TLS reverse proxy

A web server in front of an app that adds HTTPS. Lodestar needs `LODESTAR_SECURE_COOKIES=true` behind one.

## UDP

A network protocol without a connection setup, used for peer discovery. Firewall and router rules list it separately from TCP.

## VPN

A private tunnel to another network. It can hide your home IP address from peers and let you reach home safely from outside.

<!-- nav:start -->
---

[← Appendix B: Settings reference](appendix-b-settings-reference.md) · [Contents](README.md#contents) · [Revisions →](revisions.md)
<!-- nav:end -->
