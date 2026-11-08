# Companion examples

These files show what Lodestar generates for its companions on Docker and Unraid. A companion is a program Lodestar works with: qBittorrent (download client), Prowlarr (indexer manager) and gluetun (VPN client).

**Don't copy these files as they are.** Generate your own in Lodestar instead. It fills in your folders, your user and group IDs, and the pinned image for your Lodestar version, and it checks each folder first.

> [!WARNING]
> Until a release pins each image, these examples carry the placeholder digest `sha256:00…01`. No registry serves it, so Docker refuses to pull the image.

## What's here
| Folder | What it holds |
|---|---|
| `compose/` | One compose file per companion, with and without gluetun (`-gluetun`). |
| `unraid/` | The filled Unraid templates (`my-<name>.xml`). With gluetun there are two: the companion's and gluetun's. |

## What every file follows
- **Images are pinned by digest** (`image@sha256:…`), never by a tag such as `:latest`.
- **Ports stay on the Docker network** unless you publish one. The compose examples publish the web UI on `127.0.0.1` only, so only the server itself can reach it.
- **No Docker socket, no privileged mode and no host networking.** gluetun is the only one with an extra capability (`NET_ADMIN`) and a device (`/dev/net/tun`). It needs both to run the VPN tunnel and its firewall.
- **You fill in gluetun's VPN settings yourself.** Lodestar never asks for or stores a VPN key. gluetun won't start until they're real values, so nothing is sent outside the VPN in the meantime.
- **Mount real folders, never a link.** Each companion gets its own folder. It never gets Lodestar's data folder or another companion's folder.

## On Unraid
Unraid can't open a template that's already filled in from a link. Instead:

1. Save the template file to `/boot/config/plugins/dockerMan/templates-user/` on your flash drive.
2. Open **Docker → Add Container** and pick the template under **Template**.
3. Check the values, then select **Apply**.

Lodestar also lists each field's value, so you can fill in the form by hand if you prefer.

## For developers
A test generates these files and checks they match byte for byte (`crates/companions/tests/templates.rs`). After you change the generator or the catalogue, regenerate them:

```sh
UPDATE_GOLDEN=1 cargo test -p lodestar-companions --test templates
```
