#!/usr/bin/env bash
# Smoke-test a release tarball: verify its checksum, unpack it, run
# `lodestar serve` with a temporary data directory, check /healthz and that
# the web UI is embedded, then complete setup by token, see the crawler out of its setup
# hold, import a tiny fixture and find it with a search, and stop it with SIGTERM
# (exit code 0). The steps shared with the other smokes are in packaging/smoke/flow.py.
#
#   packaging/linux/smoke.sh dist/lodestar-0.5.0-x86_64-linux-musl.tar.gz [port]
set -euo pipefail

tarball=${1:?usage: smoke.sh <tarball> [port]}
port=${2:-18090}
[ -f "$tarball" ] || { echo "no such file: $tarball" >&2; exit 1; }

work=$(mktemp -d)
chmod 755 "$work"   # the server may drop to another user (PUID) and must reach the data folder
pid=
# The first-run setup token is only ever printed redacted.
redact() { sed -E 's/(setup token )[0-9A-Fa-f]{4}(-[0-9A-Fa-f]{4}){3}/\1XXXX-XXXX-XXXX-XXXX/'; }
cleanup() {
  status=$?
  [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null || true
  if [ "$status" -ne 0 ]; then
    for log in "$work/log.txt" "$work/log2.txt"; do
      [ -f "$log" ] && { echo "--- $(basename "$log") (tail, redacted)"; tail -n 60 "$log" | redact; }
    done
  fi
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

# Releases are built without the `fixture` feature (`serve --fixture` test data).
# The flag always exists; a build without the feature refuses it with exactly
# this line and exit status 2 (crates/lodestar/src/fixture.rs). Any other
# failure (a bad argument, a missing folder) must not pass for the refusal.
fixture_refusal='lodestar serve --fixture: this build has no fixture backend (feature `fixture`)'
fixture_status=0
fixture_out=$("$dir/lodestar" serve --fixture "$work/fixture" 2>&1) || fixture_status=$?
if [ "$fixture_status" -ne 2 ] || ! printf '%s\n' "$fixture_out" | grep -qxF "$fixture_refusal"; then
  printf '%s\n' "$fixture_out" >&2
  echo "::error::serve --fixture didn't refuse with the no-fixture line and exit status 2 (got $fixture_status); release builds must leave out the fixture feature" >&2
  exit 1
fi

mkdir "$work/data"
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
flow="$here/../smoke/flow.py"
url="http://127.0.0.1:$port"
dht_port=${SMOKE_DHT_PORT:-16881}

# Starts the server in the background (log: $1) and waits for /healthz.
start_server() {
  LODESTAR_DATA_DIR="$work/data" \
  LODESTAR_HTTP_BIND="127.0.0.1:$port" \
  LODESTAR_DHT_PORT="$dht_port" \
    "$dir/lodestar" serve >"$1" 2>&1 &
  pid=$!
  ok=
  for _ in $(seq 1 60); do
    if curl --fail --silent --max-time 2 "$url/healthz" >/dev/null; then ok=1; break; fi
    kill -0 "$pid" 2>/dev/null || { echo "::error::lodestar exited early" >&2; exit 1; }
    sleep 1
  done
  if [ -z "$ok" ]; then
    echo "::error::/healthz never answered" >&2; exit 1
  fi
}

# SIGTERM, and a clean exit.
stop_server() {
  kill -TERM "$pid"
  code=0
  wait "$pid" || code=$?
  pid=
  if [ "$code" != 0 ]; then
    echo "::error::lodestar exited with $code after SIGTERM" >&2; exit 1
  fi
}

start_server "$work/log.txt"
echo "healthz: $(curl --fail --silent "$url/healthz")"

# SMOKE_ALLOW_STUB=1 skips the UI check for a local build made without web/build.
if [ "${SMOKE_ALLOW_STUB:-0}" != 1 ]; then
  curl --fail --silent --max-time 10 --output "$work/index.html" "$url/"
  if grep -q "UI not built" "$work/index.html"; then
    echo "::error::/ served the stub page: the web UI was not embedded" >&2; exit 1
  fi
  grep -q '_app/' "$work/index.html" || { echo "::error::/ is not the SvelteKit index.html" >&2; exit 1; }
fi

# Setup by token (the wizard's own API), then the crawler leaves the setup hold and the DHT
# socket is bound. The smoke asks the server only about itself (the server itself may reach the
# public DHT). The token is printed redacted, and only the session cookie is kept in $work/state.
py=python3
command -v "$py" >/dev/null 2>&1 || py=python
"$py" -I "$flow" setup --url "$url" --log "$work/log.txt" --state "$work/state"
"$py" -I "$flow" crawler --url "$url" --state "$work/state" --dht
stop_server

# Import a tiny Magnetico database. `lodestar import` needs the server stopped.
"$py" -I "$flow" fixture "$work/fixture.sqlite3"
chmod 644 "$work/fixture.sqlite3"
LODESTAR_DATA_DIR="$work/data" "$dir/lodestar" import start --source magnetico --path "$work/fixture.sqlite3" \
  >"$work/import.txt" 2>&1 || { cat "$work/import.txt" >&2; echo "::error::lodestar import failed" >&2; exit 1; }
tail -n 3 "$work/import.txt"

# Start again: the imported torrents become searchable.
start_server "$work/log2.txt"
"$py" -I "$flow" search --url "$url" --state "$work/state"
stop_server
echo "smoke ok: $tarball"
