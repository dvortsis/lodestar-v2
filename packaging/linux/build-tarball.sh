#!/usr/bin/env bash
# Pack a built static `lodestar` into a release tarball plus its checksum.
#
#   packaging/linux/build-tarball.sh <rust-target> <version> <binary> <out-dir>
#   packaging/linux/build-tarball.sh x86_64-unknown-linux-musl 0.5.0 \
#       target/x86_64-unknown-linux-musl/release/lodestar dist
#
# Output, in <out-dir>:
#   lodestar-<version>-<arch>-linux-musl.tar.gz
#   lodestar-<version>-<arch>-linux-musl.tar.gz.sha256
#
# The archive has one top-level directory (same name without .tar.gz) holding
# the binary, LICENSE, README.txt and lodestar.service. The packing is
# repeatable: sorted names, a fixed timestamp, no owner or gzip name. Set
# SOURCE_DATE_EPOCH to stamp the files; it defaults to the commit time.
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 <rust-target> <version> <binary> <out-dir>" >&2
  exit 2
fi
target=$1
version=$2
binary=$3
out=$4

case "$target" in
  x86_64-unknown-linux-musl) arch=x86_64 ;;
  aarch64-unknown-linux-musl) arch=aarch64 ;;
  *) echo "unsupported target: $target (musl static builds only)" >&2; exit 2 ;;
esac
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$'; then
  echo "version must look like 1.2.3 or 1.2.3-beta.1 (no leading v): $version" >&2
  exit 2
fi
[ -f "$binary" ] || { echo "binary not found: $binary" >&2; exit 1; }

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
name="lodestar-$version-$arch-linux-musl"
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct 2>/dev/null || date +%s)}

mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir "$stage/$name"
install -m 0755 "$binary" "$stage/$name/lodestar"
install -m 0644 "$root/LICENSE" "$stage/$name/LICENSE"
install -m 0644 "$here/README.txt" "$stage/$name/README.txt"
install -m 0644 "$root/packaging/systemd/lodestar.service" "$stage/$name/lodestar.service"

(
  cd "$stage"
  tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner \
    --format=gnu -cf - "$name" | gzip -n -9 >"$out/$name.tar.gz"
)
(cd "$out" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")

echo "wrote $out/$name.tar.gz"
cat "$out/$name.tar.gz.sha256"
