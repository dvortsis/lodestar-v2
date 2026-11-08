#!/usr/bin/env bash
# Runs the fleet control plane's tests (CI: the infra job in ci.yml).
set -euo pipefail
cd "$(dirname "$0")"
python3 -m unittest discover -s tests -t . "$@"
