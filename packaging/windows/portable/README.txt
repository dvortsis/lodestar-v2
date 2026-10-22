Lodestar for Windows (portable)
===============================

No installation, no service, no changes to Windows. Everything stays in this
folder, so you can delete it to remove Lodestar.

To start: double-click "Start Lodestar.cmd". Your browser opens the web page
at http://localhost:8090/. The first-run setup token is printed in the black
window; paste it into the page to create your password. Close the window to
stop Lodestar.

Your data is in the "data" folder next to lodestar.exe. Keep this folder on
a fast local drive (SSD if you can), not on a network share or a synced
folder such as OneDrive.

Windows may ask whether to allow Lodestar through the firewall the first time.
Allow it on "Private networks" only for the web page. Lodestar does not
send data about you anywhere.

This build is not code-signed yet, so Windows SmartScreen may say "Windows
protected your PC". Check the download against the .sha256 file on the
release page, then choose "More info" > "Run anyway".

Want it to start with Windows, with firewall rules set up for you? Use the
installer (the .msi) instead.

More help: https://github.com/dvortsis/lodestar-v2/blob/main/packaging/windows/README.md
Licence: GNU AGPL-3.0-or-later (see LICENSE).
