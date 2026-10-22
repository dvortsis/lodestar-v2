# Chapter 2: Install Lodestar on Unraid and Docker

This chapter installs Lodestar from the Unraid template or with Docker Compose, then takes you through first run.

> [!NOTE]
> **What you will learn**
> - Install Lodestar from the Unraid template with the data folder on a pool.
> - Set the template fields to their recommended values.
> - Run Lodestar as the right user so it can write its data.
> - Install with Docker Compose.
> - Create the admin password with the [setup token](glossary.md#setup-token).

## 2.1 Install with the Unraid template
<!-- index: Unraid; template; Community Applications -->

Lodestar's template is `packaging/unraid/lodestar.xml`. Community Applications doesn't list Lodestar yet **(planned)**, so you add the template by hand once.

To install from the template:

1. Open the Unraid terminal (the **>_** icon at the top right of the web UI).
2. Paste this command to download the template:

    ```bash
    wget -O /boot/config/plugins/dockerMan/templates-user/lodestar.xml https://github.com/dvortsis/lodestar-v2/raw/main/packaging/unraid/lodestar.xml
    ```

   **Result:** The command ends without an error.
3. Open the **Docker** tab and select **Add Container**.
4. In **Template**, choose **Lodestar** under **User templates**.
   **Result:** The form shows **Web UI**, **DHT / peers (UDP)** and **Data**.
5. Check the fields against [section 2.2](#22-settings-in-the-unraid-template). Leave everything else on its default.
6. Select **Apply**.
   **Result:** Unraid pulls the image and starts the [container](glossary.md#container). A **Lodestar** icon appears on the Docker tab.

Then set the file owner in [section 2.3](#23-set-the-user-the-container-runs-as) and finish with [section 2.5](#25-create-the-admin-password).

> [!WARNING]
> Don't choose the **Lodestar-gluetun** template unless a gluetun [VPN](glossary.md#vpn) container already runs. It has no network of its own. [Chapter 3](03-keep-lodestar-safe.md#33-decide-about-the-dht-port-and-a-vpn) explains it.

## 2.2 Settings in the Unraid template
<!-- index: template fields; appdata; Intensity -->

Fields marked *advanced* appear after you select **Show more settings**.

### Web UI

The port your browser uses to open Lodestar. The template maps host port `8090` to container port `8090`.

**Recommended:** `8090`. Change the host side only if another app uses it. Keep the container side at `8090`.

**Unsafe:** Forwarding this port on your router. Keep it on your home network and use a VPN instead. [Chapter 3](03-keep-lodestar-safe.md#31-keep-the-web-ui-off-the-internet) says why.

**Safe to change later:** Yes. Edit the container, change the host port and apply.

### DHT / peers (UDP)

The port Lodestar uses to talk to the [DHT](glossary.md#dht). It is [UDP](glossary.md#udp), not TCP.

**Recommended:** `6881`. You don't need to forward it on your router. Forwarding is optional and has a trade-off ([section 3.3](03-keep-lodestar-safe.md#33-decide-about-the-dht-port-and-a-vpn)).

**Safe to change later:** Yes.

### Peers (TCP) (advanced)

Reserved for incoming peer connections **(planned)**. Lodestar only makes outbound fetches today, so nothing listens here.

**Recommended:** leave it on `6881`. Don't forward it.

### Data

The folder where Lodestar keeps its index, database and settings. It is the one folder that survives an update, because a container is rebuilt on every update.

**Recommended:** `/mnt/cache/appdata/lodestar`. Use a pool path (`/mnt/cache/...` or `/mnt/<pool>/...`), not `/mnt/user/...`. The `/mnt/user` path goes through Unraid's FUSE layer, which is slow for this workload and risky for SQLite.

**Unsafe:** A path on the array or under `/mnt/user`. Use your cache pool instead.

**Safe to change later:** Only by moving the folder. Stop the container, copy the folder, change the field and start it.

### Hot path (optional) (advanced) and Cold path (optional) (advanced)

Planned, not used yet. They will split the index and the file-tree blobs across pools.

**Recommended:** leave both empty. Everything stays under **Data**.

### PUID and PGID (advanced)

The template shows them, but the image doesn't read them yet. See [section 2.3](#23-set-the-user-the-container-runs-as).

**Recommended:** leave `99` and `100`, and add the extra parameter in section 2.3.

### TZ (advanced)

The time zone for log timestamps. Log timestamps are UTC today whatever you set.

**Recommended:** leave `Etc/UTC`.

### Intensity (advanced)

How hard the crawler works: `low`, `balanced` or `aggressive`. Balanced opens about 400 [metadata](glossary.md#metadata) connections at once. Aggressive opens about 1000.

**Recommended:** `balanced`. Start at `low` if your network slows down.

**Unsafe:** `aggressive` can fill a home router's connection table, which slows the whole household. Use `balanced` or `low` instead.

**Safe to change later:** Yes. Apply the change and the container restarts.

### Log level (advanced)

How much Lodestar writes to the log: `error`, `warn`, `info` or `debug`.

**Recommended:** `info`. Logs hold no torrent details by default.

## 2.3 Set the user the container runs as
<!-- index: PUID; PGID; file owner; permissions -->

The container runs as a user ID. If that user can't write to the data folder, Lodestar exits with a permission error. The image defaults to user `65532`, and the template's **PUID** and **PGID** fields aren't read yet. Unraid's appdata folders belong to user `99` (nobody) and group `100` (users).

To run Lodestar as nobody:users:

1. On the Docker tab, select the **Lodestar** icon, then **Edit**.
2. Turn on **Advanced View** at the top right.
3. In **Extra Parameters**, enter `--user 99:100`.
4. Select **Apply**.
   **Result:** The container restarts and can write to `/mnt/cache/appdata/lodestar`.

If the log still shows a permission error creating files in `/data`, the folder belongs to another user. Open the Unraid terminal and paste:

```bash
chown -R 99:100 /mnt/cache/appdata/lodestar
```

This changes the owner of the Lodestar data folder only. Change the path if you chose another pool.

## 2.4 Install with Docker Compose
<!-- index: Docker Compose; named volume; docker-compose.yml -->

Docker Compose runs Lodestar from one file, `docker-compose.yml`. The file maps the web UI, the DHT port and a **named volume** for `/data`.

| Option | When to use it |
|---|---|
| **Named volume (recommended)** | The default in the file. Docker creates it with the right owner and keeps it across updates. |
| Bind mount | You want the data in a folder you pick. You must make that folder writable by the container's user. |

To install with the named volume:

1. In a terminal on your Docker host, make a folder and move into it:

    ```bash
    mkdir lodestar && cd lodestar
    ```

2. Download the compose file:

    ```bash
    curl -fsSLO https://raw.githubusercontent.com/dvortsis/lodestar-v2/main/docker-compose.yml
    ```

3. Start Lodestar:

    ```bash
    docker compose up -d
    ```

   **Result:** Docker pulls `ghcr.io/dvortsis/lodestar-v2:latest` and starts the container `lodestar`.
4. Check that it is healthy:

    ```bash
    docker inspect --format '{{.State.Health.Status}}' lodestar
    ```

   **Result:** The command prints `healthy` after about 30 seconds.

> [!WARNING]
> Keep the data on local SSD or NVMe storage. Don't put the volume on a network share or a FUSE filesystem. The database is SQLite and misbehaves there.

To use a bind mount owned by user `1000`, run `mkdir data && sudo chown 1000:1000 data`. Then change `user:` to `"1000:1000"` and the volume line to `- ./data:/data`.

Do not use `network_mode: host` unless you need it. It skips Docker's network translation for better DHT reach, but it exposes port `8090` on every host interface.

Pin an exact version such as `x.y.z` instead of `latest` if you want updates only when you decide. `nightly` is untested between releases.

## 2.5 Create the admin password
<!-- index: setup token; admin password; first run -->

On a fresh data folder, Lodestar prints a one-time **setup token** to its log. Anyone who can read the log can claim a fresh instance, so do this before anyone else can reach the web UI.

To create the admin password:

1. Read the log. On Unraid, select the **Lodestar** icon on the Docker tab, then **Logs**. With Compose, run this on the Docker host:

    ```bash
    docker compose logs lodestar
    ```

2. Find the line that starts `first run: open` and copy the token at its end.
3. Open `http://<server-ip>:8090` in your browser. Replace `<server-ip>` with your server's address, or use `localhost` on the same machine.
4. Enter the token and choose a password from your password manager.
   **Result:** Lodestar signs you in. It prints no token again.

The crawler starts at once. The library fills slowly at first while the DHT routing table warms up.

> [!NOTE]
> **Worked example**
> You lost the token. While no admin exists, every start prints a fresh token. Restart the container, read the log again and use the newest token.

## 2.6 Check your understanding

1. Which Unraid path do you use for **Data**, and why not `/mnt/user/appdata/lodestar`?
2. The log shows a permission error creating files in `/data`. What do you change?
3. When does the setup token stop being needed?

> [!TIP]
> **Key takeaways**
> - Map **Data** to a pool path such as `/mnt/cache/appdata/lodestar`.
> - Add `--user 99:100` on Unraid; the PUID and PGID fields aren't read yet.
> - Create the admin password straight after the first start.

<!-- nav:start -->
---

[← Chapter 1: Before you start](01-before-you-start.md) · [Contents](README.md#contents) · [Chapter 3: Keep Lodestar safe →](03-keep-lodestar-safe.md)
<!-- nav:end -->
