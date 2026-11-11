#!/usr/bin/env bash
# copy-off (OP-3b; plan "Raw logs, credentials and databases stay on the VMs"):
# the block's scrubbed evidence, and the block's end.
# - Refuses while Lodestar or a collector runs (fail closed).
# - Runs analyze_cr.py on each segment and the allow-list scrubber over the
#   results, in a throwaway container with no network that sees the evidence
#   read-only. Raw logs, databases, executables and the password never leave.
# - Prints the scrubbed document as one `evidence <base64>` line, which the
#   control side scrubs again and puts only in the private evidence bucket.
# - Then ends the block: the marker goes (a block that stopped early ends here
#   too) and the watcher stops. Re-running it copies the same evidence again.
# Run as root by the step bootstrap: step.sh <workdir> [-] [-] [block].
set -uo pipefail
work=${1:?workdir}
BLOCK=${4:-/var/lib/lodestar-block}
# shellcheck source=block-lib.sh
. "$work/block-lib.sh"

fail() { echo "fail $1"; echo "fail copy_off"; exit 1; }

command -v docker >/dev/null 2>&1 || fail tool_missing_docker
state=$(block_state)
[[ $state == unknown ]] && fail block_check
block_procs; rc=$?
((rc == 2)) && fail block_check
((rc == 0)) && fail block_running
[[ -d $BLOCK/evidence && -f $BLOCK/block.env && -f $BLOCK/tools/analyze_cr.py ]] || fail evidence_present
load_block_env || fail block_env
echo "ok nothing_running"

rm -rf -- "$work/out" && mkdir -m 0700 -- "$work/out" || fail scrubbed
# Only what the scrubber reads is mounted (never the password).
mounts=()
for f in evidence block.env tools outages notes; do
  [[ -e $BLOCK/$f ]] && mounts+=(-v "$BLOCK/$f:/block/$f:ro")
done
doc=$(timeout 600 docker run --rm --network none --cap-drop ALL --security-opt no-new-privileges:true \
  --read-only --tmpfs /tmp "${mounts[@]}" -v "$work/scrub.py:/scrub.py:ro" -v "$work/out:/out" \
  "$COLLECTOR_IMAGE" python3 /scrub.py collect /block /out 2>/dev/null) || fail scrubbed
[[ $doc == \{* && ${#doc} -le 196608 ]] || fail scrubbed
echo "ok scrubbed"
segs=$(printf '%s' "$doc" | grep -oE '"segment": ?[1-4]' | grep -c .)
echo "count segments_copied ${segs:-0}"
echo "evidence $(printf '%s' "$doc" | base64 -w0)"

if [[ $state == block ]]; then
  stop_watcher
  rm -f -- "$BLOCK/marker" && sync "$BLOCK" || fail block_ended
  echo "ok block_ended"
else
  echo "skip block_ended"
fi
echo "ok copy_off"
