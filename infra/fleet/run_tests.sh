#!/usr/bin/env bash
# Runs the fleet control plane's tests (CI: the infra job in ci.yml), and PIA
# delivery's (infra/pia-bridge, OP-2b) and the operator bridge's (infra/operator,
# OP-4), which build on the same modules, and the test VM tool's (infra/dev-vm,
# X-test-vm).
set -euo pipefail
cd "$(dirname "$0")"
python3 -m unittest discover -s tests -t . "$@"
(cd ../pia-bridge && python3 -m unittest discover -s tests -t . "$@")
(cd ../operator && python3 -m unittest discover -s tests -t . "$@")
(cd ../dev-vm && python3 -m unittest discover -s tests -t . "$@")
