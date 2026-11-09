#!/usr/bin/env bash
# Runs the fleet control plane's tests (CI: the infra job in ci.yml), and PIA
# delivery's (infra/pia-bridge, OP-2b), which builds on the same modules.
set -euo pipefail
cd "$(dirname "$0")"
python3 -m unittest discover -s tests -t . "$@"
(cd ../pia-bridge && python3 -m unittest discover -s tests -t . "$@")
