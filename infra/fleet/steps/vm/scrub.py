"""The copy-off scrubber (OP-3b; docs/plan/2026-11-08-cloud-operator.md, "Raw
logs, credentials and databases stay on the VMs").

An allow-list, not a filter. Only the named fields of the collector's
`result.json` and of `analyze_cr.py segment`'s report survive, and each value
must also have its field's shape: a number, a boolean, a fixed-vocabulary word,
a UTC time, a rule number, a commit or the image digest, in the fields that
hold those. A field that isn't named, or a value with the wrong shape, is
dropped. Free text never passes, so names, infohashes, peer and VM addresses,
instance and Linode IDs, PIA server names and the region can't.

It runs twice: on the VM (`scrub.py collect <evidence> <out>`, in a throwaway
container with no network), and again in the control plane on what came back
(`scrub(doc)`), before anything goes to the private evidence bucket.
Standard library only.
"""

from __future__ import annotations

import json
import math
import re
import subprocess
import sys
from pathlib import Path

MAX_BYTES = 192 * 1024
_WORD = re.compile(r"[a-z][a-z0-9_]{0,63}\Z")
_HEXLIKE = re.compile(r"[0-9a-f]{16,}\Z")  # an infohash or an ID, even one that starts with a letter
_TIME = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?Z\Z")
_RULE = re.compile(r"[0-9]{1,2}(\.[0-9]{1,2}){0,2}[a-z]?\Z")
_COMMIT = re.compile(r"[0-9a-f]{40}\Z")
_DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")


def num(v):
    return (isinstance(v, int) and not isinstance(v, bool) and abs(v) < 10**15) or \
        (isinstance(v, float) and math.isfinite(v))


def word(v):
    return isinstance(v, str) and bool(_WORD.match(v)) and not _HEXLIKE.match(v)


def boolean(v):
    return isinstance(v, bool)


def time(v):
    return isinstance(v, str) and bool(_TIME.match(v))


def rule(v):
    return isinstance(v, str) and bool(_RULE.match(v))


def commit(v):
    return isinstance(v, str) and bool(_COMMIT.match(v))


def digest(v):
    return isinstance(v, str) and bool(_DIGEST.match(v))


def scalar(v):
    return v is None or num(v) or boolean(v) or word(v) or time(v)


def one_of(*words):
    return lambda v: v in words


def opt(check):
    return lambda v: v is None or check(v)


SWITCHES = {k: opt(boolean) for k in ("safe_restart", "coordinated_admission", "responder_hints",
                                       "streaming_peers", "target_prefixes", "utp", "route_backoff")}
RESULT = {
    "schema_version": num, "state": word, "reason": word, "stop_mode": word, "samples": num, "ended": time,
    "suppression_ended": opt(lambda v: num(v) or time(v)), "baseline_started": opt(lambda v: num(v) or time(v)),
    "elapsed_seconds": num,
    "finalization_complete": boolean, "publication_failures": [word], "working_set_over_1GiB_samples": num,
    "network_parent_changes": num,
    "shutdown": {"stop_mode": word, "exit_code": opt(num), "exit_confirmed": boolean, "signal_delivered": boolean},
}
REPORT = {
    "schema": num, "kind": one_of("ab", "comb", "comparator"), "treatment": opt(one_of("adm", "utp", "route")), "rehearsal": boolean, "window_hours": num,
    "hour_seconds": num, "state": word, "samples": num, "clean_stop": boolean, "verdict": one_of("valid", "invalid", "void"),
    "verdict_reasons": [word],
    "checks": [{"rule": rule, "check": word, "ok": boolean, "effect": word}],
    "identity": {"image_digest": opt(digest), "switches": SWITCHES},
    "numerator": {"window": {k: opt(num) for k in ("gross", "carry_in_bound", "carry_in_share", "net", "minutes",
                                                   "mean_net_per_min", "mean_gross_per_min", "stored_per_min")},
                  "worst_hour_net_per_min": opt(num)},
    "latency": {"measured": boolean, "p95_seconds": opt(num)},
    "fetch": {k: opt(num) for k in ("attempts", "successes", "success_per_attempt", "connects_per_second")},
    "resources": {k: opt(num) for k in ("peak_private_bytes", "peak_working_set_bytes", "cpu_cores_mean")},
    "activity_notes": [{"category": word}],
}
SEGMENT = {"segment": num, "arm": one_of("control", "treatment"), "result": RESULT, "report": REPORT}
DOC = {"version": num, "rules_commit": commit, "spec_commit": commit, "lodestar_commit": commit,
       "image_digest": digest, "treatment": one_of("adm", "utp", "route"), "segment_hours": num,
       "role": one_of("main", "worker"),
       "outages": num, "outage_notes": num, "segments": [SEGMENT]}
MAX_ITEMS = 200


DROP = object()


def clean(value, spec):
    """The allow-listed part of value under spec, or DROP when it doesn't fit."""
    if isinstance(spec, dict):
        if not isinstance(value, dict):
            return DROP
        out = {}
        for k, sub in spec.items():
            if k in value:
                v = clean(value[k], sub)
                if v is not DROP:
                    out[k] = v
        return out
    if isinstance(spec, list):
        if not isinstance(value, list):
            return DROP
        return [v for v in (clean(x, spec[0]) for x in value[:MAX_ITEMS]) if v is not DROP]
    return value if spec(value) else DROP


def scrub(doc) -> dict:
    """The control plane's pass, on what a VM sent back."""
    out = clean(doc, DOC)
    if out is DROP:
        raise ValueError("not a copy-off document")
    return out


def dumps(doc) -> bytes:
    text = json.dumps(doc, separators=(",", ":"), sort_keys=True, allow_nan=False).encode()
    if len(text) > MAX_BYTES:
        raise ValueError("scrubbed evidence is too large")
    return text


def read_json(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def count_lines(path: Path) -> int:
    try:
        return sum(1 for ln in path.read_text(encoding="ascii", errors="replace").splitlines() if ln.strip())
    except OSError:
        return 0


def collect(block: Path, out: Path, run=subprocess.run) -> dict:
    """The raw document from a block directory (`evidence/seg-N`, `block.env`,
    `tools/analyze_cr.py`), scrubbed. Reports are written to out, never into
    the evidence."""
    env = {}
    for line in (block / "block.env").read_text(encoding="ascii").splitlines():
        k, _, v = line.partition("=")
        env[k] = v
    doc = {"version": 1, "rules_commit": env.get("RULES_COMMIT"), "spec_commit": env.get("SPEC_COMMIT"),
           "lodestar_commit": env.get("LODESTAR_COMMIT"), "image_digest": "sha256:" + env.get("IMAGE_DIGEST", ""),
           "treatment": env.get("TREATMENT"), "segment_hours": int(env.get("SEGMENT_HOURS", "0") or 0),
           "role": env.get("ROLE"), "outages": count_lines(block / "outages"),
           "outage_notes": count_lines(block / "notes"), "segments": []}
    for n in range(1, 5):
        seg = block / "evidence" / f"seg-{n}"
        if not seg.is_dir():
            continue
        report_path = out / f"seg-{n}.json"
        try:  # four segments at 120 s each fit copy-off's 600 s
            r = run([sys.executable, str(block / "tools" / "analyze_cr.py"), "segment", str(seg),
                     "--output", str(report_path)], capture_output=True, timeout=120, check=False)
            report = read_json(report_path) if r.returncode == 0 else None
        except subprocess.TimeoutExpired:
            report = None
        doc["segments"].append({"segment": n, "arm": "treatment" if n in (2, 3) else "control",
                                "result": read_json(seg / "result.json"),
                                "report": report if report is not None else {"state": "analysis_failed"}})
    return scrub(doc)


def main(argv: list[str]) -> int:
    if len(argv) != 3 or argv[0] != "collect":
        return 2
    try:
        sys.stdout.buffer.write(dumps(collect(Path(argv[1]), Path(argv[2]))) + b"\n")
    except (OSError, ValueError):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
