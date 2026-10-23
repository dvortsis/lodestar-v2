Lodestar for Linux
==================

This folder holds a single static program, `lodestar`. It needs no other
packages. The web page is built into it.

Try it first (nothing is installed, and it keeps its data in ./data):

    ./lodestar serve

Then open http://localhost:8090/ in a browser. The first-run setup token is
printed in the terminal; paste it into the page to create your password.

Run it as a service (recommended; starts at boot, runs as an unprivileged user):

    sudo install -m 0755 lodestar /usr/local/bin/lodestar
    sudo install -m 0644 lodestar.service /etc/systemd/system/lodestar.service
    sudo systemctl daemon-reload
    sudo systemctl enable --now lodestar
    journalctl -u lodestar | grep "setup token"

Ports: 8090/tcp is the web page; keep it on your home network. 6881/udp is the
DHT; forward it on your router for the best results.

Check the download before you run it: `sha256sum -c <file>.tar.gz.sha256`
(next to the tarball on the release page).

More help: https://github.com/dvortsis/lodestar-v2/blob/main/packaging/linux/README.md
Licence: GNU AGPL-3.0-or-later (see LICENSE).
