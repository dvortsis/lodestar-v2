#!/usr/bin/env bash
# Smoke-test a release tarball: verify its checksum, unpack it, run
# `lodestar serve` with a temporary data directory, check /healthz and that
# the web UI is embedded, then stop it with SIGTERM and expect exit code 0.
#
#   packaging/linux/smoke.sh dist/lodestar-0.5.0-x86_64-linux-musl.tar.gz [port]
set -euo pipefail

tarball=${1:?usage: smoke.sh <tarball> [port]}
port=${2:-18090}
[ -f "$tarball" ] || { echo "no such file: $tarball" >&2; exit 1; }

work=$(mktemp -d)
pid=
cleanup() {
  [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

(cd "$(dirname "$tarball")" && sha256sum --check "$(basename "$tarball").sha256")

tar -xzf "$tarball" -C "$work"
dir=$(find "$work" -mindepth 1 -maxdepth 1 -type d)
for f in lodestar LICENSE README.txt lodestar.service; do
  [ -f "$dir/$f" ] || { echo "::error::tarball is missing $f" >&2; exit 1; }
done
[ -x "$dir/lodestar" ] || { echo "::error::lodestar is not executable" >&2; exit 1; }
if command -v file >/dev/null 2>&1 && file "$dir/lodestar" | grep -q 'dynamically linked'; then
  echo "::error::lodestar is dynamically linked; the tarball must be static" >&2
  exit 1
fi

mkdir "$work/data"
LODESTAR_DATA_DIR="$work/data" \
LODESTAR_HTTP_BIND="127.0.0.1:$port" \
LODESTAR_DHT_ENABLED=false \
  "$dir/lodestar" serve >"$work/log.txt" 2>&1 &
pid=$!

url="http://127.0.0.1:$port"
for _ in $(seq 1 60); do
  if curl --fail --silent --max-time 2 "$url/healthz" >/dev/null; then ok=1; break; fi
  kill -0 "$pid" 2>/dev/null || { echo "::error::lodestar exited early" >&2; cat "$work/log.txt"; exit 1; }
  sleep 1
done
if [ -z "${ok:-}" ]; then
  echo "::error::/healthz never answered" >&2; cat "$work/log.txt"; exit 1
fi
echo "healthz: $(curl --fail --silent "$url/healthz")"

# SMOKE_ALLOW_STUB=1 skips the UI check for a local build made without web/build.
if [ "${SMOKE_ALLOW_STUB:-0}" != 1 ]; then
  curl --fail --silent --max-time 10 --output "$work/index.html" "$url/"
  if grep -q "UI not built" "$work/index.html"; then
    echo "::error::/ served the stub page: the web UI was not embedded" >&2; exit 1
  fi
  grep -q '_app/' "$work/index.html" || { echo "::error::/ is not the SvelteKit index.html" >&2; exit 1; }
fi

kill -TERM "$pid"
code=0
wait "$pid" || code=$?
pid=
if [ "$code" != 0 ]; then
  echo "::error::lodestar exited with $code after SIGTERM" >&2; cat "$work/log.txt"; exit 1
fi
echo "smoke ok: $tarball"
