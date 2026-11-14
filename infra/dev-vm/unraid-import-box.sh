#!/bin/bash
# The owner's one command for the real-dump import test on the Unraid server
# (X-test-vm; owner 2026-11-14 "You can run the archive importer testing on the
# Unraid machine"; runner booking of one ext:import-host run, owner-decisions §2.52).
#
# Paste into the Unraid web terminal (as root). It builds a throwaway container:
#   - the dumps folder mounted READ-ONLY at /dumps;
#   - a new, empty work folder on a direct disk or pool (not /mnt/user, which
#     makes PostgreSQL much slower; bitmagnet-restore.md step 1) at /work;
#   - no Docker socket, not privileged, no host network or PID, no other appdata,
#     no credentials; a memory limit so the rest of the server keeps running;
#   - Claude Code inside, started with Remote Control so the coordinator can brief it.
# It never touches parity, array settings, other containers or other shares.
#
#   bash unraid-import-box.sh            set up and open Claude (sign in once)
#   bash unraid-import-box.sh attach     open Claude again later
#   bash unraid-import-box.sh remove     delete the container and the work folder
set -euo pipefail

DUMPS=${DUMPS:-/mnt/user/data/dht_crawler}
NAME=lodestar-import-test
IMAGE=postgres:16-bookworm      # PostgreSQL 16 (bitmagnet-restore.md) on Debian
MIN_FREE_GB=250                 # restored cluster (~100 GiB) + stores + a Magnetico copy
STATE=/root/.lodestar-import-box   # remembers the work folder for "remove"
SESSION_NAME='lodestar-import-unraid'
FIRST="You're the import test session in a throwaway container on the owner's Unraid server, for the X-test-vm coordinator. Wait for the coordinator's brief before doing anything."

say() { printf '%s\n' "$*"; }
die() { printf 'Stopped: %s\n' "$*" >&2; exit 1; }

attach() {
  docker exec -it "$NAME" su - tester -c 'tmux attach -t claude'
}

case "${1:-setup}" in
  attach) attach; exit ;;
  remove)
    W=$(cat "$STATE" 2>/dev/null || true)
    say "This deletes the container '$NAME'${W:+ and the work folder $W}. The dumps in $DUMPS are not touched."
    read -r -p "Type yes to continue: " a; [ "$a" = yes ] || die "nothing removed"
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    if [ -n "$W" ] && [ -d "$W" ]; then
      case "$W" in /mnt/*/lodestar-import-work) rm -rf -- "$W" ;; *) die "unexpected work folder '$W'; delete it by hand" ;; esac
    fi
    rm -f "$STATE"
    say "Removed. You can also delete the image with: docker rmi $IMAGE"
    exit ;;
  setup) ;;
  *) die "unknown command '$1' (use: attach, remove, or nothing)" ;;
esac

# --- Checks: read-only, nothing changes until you confirm. ---
command -v docker >/dev/null || die "Docker isn't running. Start it in Settings > Docker."
[ -d "$DUMPS" ] || die "dumps folder $DUMPS not found (run with DUMPS=/path bash $0)"
if docker inspect "$NAME" >/dev/null 2>&1; then
  say "The container already exists; opening Claude."; attach; exit
fi
if [ -r /proc/mdstat ]; then
  pos=$(awk -F= '/^mdResyncPos=/{print $2}' /proc/mdstat)
  [ "${pos:-0}" = 0 ] || die "a parity check, sync or rebuild is running. Run this after it finishes."
fi
others=$(docker ps --format '{{.Names}}' | grep -i lodestar || true)
if [ -n "$others" ]; then
  say "These Lodestar containers are running:"; say "$others"
  say "The import test must not overlap a soak or another Lodestar run."
  read -r -p "Continue anyway? [y/N] " a; case "$a" in [yY]*) ;; *) die "nothing changed" ;; esac
fi

# Work folder: the cache pool if it has room, else the array disk with the most free space.
pick=""; best=0
for d in /mnt/cache /mnt/disk[0-9]*; do
  [ -d "$d" ] || continue
  free=$(df -BG --output=avail "$d" 2>/dev/null | tail -1 | tr -dc 0-9)
  [ -n "$free" ] || continue
  if [ "$d" = /mnt/cache ] && [ "$free" -ge "$MIN_FREE_GB" ]; then pick=$d; best=$free; break; fi
  if [ "$free" -gt "$best" ]; then pick=$d; best=$free; fi
done
[ -n "${WORK_DISK:-}" ] && { pick=$WORK_DISK; best=$(df -BG --output=avail "$pick" | tail -1 | tr -dc 0-9); }
[ -n "$pick" ] || die "no /mnt/cache or /mnt/diskN found (run with WORK_DISK=/mnt/yourpool bash $0)"
W="$pick/lodestar-import-work"
[ -e "$W" ] && die "$W already exists. Remove it first, or run: bash $0 remove"
[ "$best" -ge "$MIN_FREE_GB" ] || say "Warning: $pick has ${best} GB free; the full test wants about ${MIN_FREE_GB} GB. It will stop at 1M if space runs short."

mem_kb=$(awk '/^MemTotal:/{print $2}' /proc/meminfo)
mem_gb=$(( mem_kb / 1024 / 1024 / 2 )); [ "$mem_gb" -gt 24 ] && mem_gb=24; [ "$mem_gb" -lt 6 ] && mem_gb=6

say ""
say "Ready to create the test container:"
say "  dumps (read-only):  $DUMPS"
say "  work folder (new):  $W   (${best} GB free)"
say "  memory limit:       ${mem_gb} GB of this server's RAM"
say "  removal later:      bash $0 remove"
read -r -p "Create it? [y/N] " a; case "$a" in [yY]*) ;; *) die "nothing changed" ;; esac

# --- Create. ---
mkdir -p "$W"; printf '%s\n' "$W" > "$STATE"
docker run -d --name "$NAME" --hostname lodestar-import \
  --memory "${mem_gb}g" --memory-swap "${mem_gb}g" --cpu-shares 512 --pids-limit 4096 \
  --security-opt no-new-privileges \
  --mount "type=bind,src=$DUMPS,dst=/dumps,readonly" \
  --mount "type=bind,src=$W,dst=/work" \
  --entrypoint sleep "$IMAGE" infinity >/dev/null

say "Installing the tools inside the container (about 5 minutes)..."
docker exec -i -e FIRST="$FIRST" -e SESSION_NAME="$SESSION_NAME" "$NAME" bash -s <<'IN'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends ca-certificates curl git tmux build-essential pkg-config \
  unzip xz-utils procps time >/dev/null
id tester >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash tester
chown tester: /work
cat >> /home/tester/.profile <<'P'
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/usr/lib/postgresql/16/bin:$PATH"
P
su - tester -c 'set -e
  curl --proto "=https" --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path >/dev/null
  curl -fsSL https://claude.ai/install.sh | bash >/dev/null
  [ -d ~/lodestar-v2 ] || git clone -q https://github.com/dvortsis/lodestar-v2.git ~/lodestar-v2'
{
  echo 'cd "$HOME/lodestar-v2"'
  echo "claude --remote-control $(printf %q "$SESSION_NAME") --permission-mode auto $(printf %q "$FIRST")"
  echo 'exec bash'
} > /home/tester/.start-claude.sh
chown tester: /home/tester/.start-claude.sh
su - tester -c 'tmux new-session -d -s claude "bash -l $HOME/.start-claude.sh"'
IN

cat <<EOT

Claude is starting inside the container. In the next screen:
  1. If it asks, pick a theme and sign in: open the link it shows, sign in,
     copy the code and paste it back here.
  2. If it asks whether to trust the folder, choose yes.
  3. Its first message is already filled in. Then tell the coordinator in your
     Claude app that the Unraid session is up.
  4. To leave it running: press Ctrl+B, let go, then press D.
To open it again later: bash $0 attach
EOT
read -r -p "Press Enter to open Claude... " _
attach
