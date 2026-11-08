#!/usr/bin/env python3
"""The signed update manifest's builder (architecture §12.2, PK-manifest).

`crates/update/README.md` is the contract: this writes a format-1
`lodestar-manifest` body and the `<channel>.json` envelope that
`.github/workflows/freshness.yml` reads and re-signs. It never signs
and never touches a key: release.yml signs the body with the minisign
CLI between `build` and `envelope`.

Usage:
  update_manifest.py build --channel C --version V --repo OWNER/NAME --tag T
      --x86-64-binary P --aarch64-binary P --msi P --portable P
      --statements FILE --out FILE
      [--previous BODY] [--metadata FILE] [--notes-file FILE]
      [--notes-url URL] [--keep N] [--now SECONDS]
  update_manifest.py extract ENVELOPE BODY_OUT MINISIG_OUT
  update_manifest.py envelope BODY MINISIG OUT
  update_manifest.py names BODY

Every artifact URL is under `https://github.com/<repo>/releases/download/`,
and this release's under its own tag (`v<version>`; `nightly` for a
nightly). A release without a pre-release part needs `--metadata` (its
level and flags); a pre-release without one gets the most cautious values
and a warning.

`build` exits 3, writing nothing, when `--previous` already lists the
version: a re-run never replaces a published release's entry. Every
other refusal exits 1 with the reason; output past a cap is refused,
never cut.

Standard library only.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import re
import sys
import time
from pathlib import Path
from urllib.parse import urlsplit

# The README's caps (crates/update/src/doc.rs).
MAX_ENVELOPE = 512 * 1024
MAX_MANIFEST = 384 * 1024
MAX_STATEMENT = 2 * 1024
MAX_STATEMENTS = 64
MAX_RELEASES = 64
MAX_ARTIFACTS = 8
MAX_NOTES = 16 * 1024
MAX_VERSION = 64
MAX_URL = 512
MAX_ARTIFACT_SIZE = 256 * 1024 * 1024
MAX_LIFETIME = 30 * 86400
DOWNLOAD_HOSTS = ("github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com")
TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl", "x86_64-pc-windows-msvc")
KINDS = ("binary", "msi", "portable")
LEVELS = ("fix", "feature", "major")
CHANNELS = ("stable", "beta", "nightly")
EXIT_ALREADY = 3

# Without a metadata file the release gets the values that install
# automatically least often and check for the most free space.
CONSERVATIVE = {
    "level": "major",
    "security": False,
    "manual_steps": False,
    "min_from": None,
    "migrates": True,
    "reindex": True,
    "reindex_bytes": 0,
}
RELEASE_FIELDS = ("version", "date", "level", "security", "manual_steps", "min_from", "withdrawn",
                  "migrates", "reindex", "reindex_bytes", "notes", "notes_url", "artifacts")
ARTIFACT_FIELDS = ("target", "kind", "url", "sha256", "size")
REPO = re.compile(r"^[A-Za-z0-9_-][A-Za-z0-9_.-]*/[A-Za-z0-9_-][A-Za-z0-9_.-]*$")
SAFE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?$"
)


class Refused(Exception):
    """The input or the output breaks the contract."""


def version_key(v: str) -> tuple:
    """Semver 2.0 precedence; `v` already passed `check_version`."""
    m = SEMVER.match(v)
    core = tuple(int(m.group(i)) for i in (1, 2, 3))
    if m.group(4) is None:
        return core, (1,)
    ids = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in m.group(4).split("."))
    return core, (0, ids)


def check_version(v: object, what: str = "version") -> str:
    if not isinstance(v, str) or len(v.encode()) > MAX_VERSION or not SEMVER.match(v):
        raise Refused(f"{what} {v!r} is not strict semver without build metadata, at most {MAX_VERSION} bytes")
    return v


def check_url(url: object, hosts: tuple[str, ...] | None, what: str) -> str:
    if not isinstance(url, str) or len(url.encode()) > MAX_URL or not url.isascii():
        raise Refused(f"{what} must be an ASCII URL of at most {MAX_URL} bytes")
    parts = urlsplit(url)
    host = parts.hostname or ""
    if (parts.scheme != "https" or parts.netloc != host or host != host.lower() or not host
            or parts.query or parts.fragment or "?" in url or "#" in url or "@" in url):
        raise Refused(f"{what} {url!r} must be https with a lower-case host, no user, port, query or fragment")
    if hosts is not None and host not in hosts:
        raise Refused(f"{what} {url!r} is not on the download allowlist {hosts}")
    return url


def check_int(v: object, what: str, lo: int = 0, hi: int = 2**64 - 1) -> int:
    if type(v) is not int or not lo <= v <= hi:
        raise Refused(f"{what} must be an integer from {lo} to {hi}")
    return v


def check_bool(v: object, what: str) -> bool:
    if type(v) is not bool:
        raise Refused(f"{what} must be true or false")
    return v


def check_fields(obj: object, fields: tuple[str, ...], optional: tuple[str, ...], what: str) -> dict:
    if not isinstance(obj, dict):
        raise Refused(f"{what} must be an object")
    unknown = sorted(set(obj) - set(fields))
    missing = sorted(set(fields) - set(optional) - set(obj))
    if unknown or missing:
        raise Refused(f"{what}: unknown fields {unknown}, missing fields {missing}")
    return obj


def artifact(target: str, kind: str, path: Path, url_base: str) -> dict:
    """An artifact entry from a real file: its URL, SHA-256 and size."""
    if not path.is_file():
        raise Refused(f"the {kind} for {target} is missing: {path}")
    if not SAFE_NAME.match(path.name):
        raise Refused(f"{path.name!r} isn't a safe release asset name")
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as f:
        while chunk := f.read(1 << 20):
            digest.update(chunk)
            size += len(chunk)
    return {
        "target": target,
        "kind": kind,
        "url": url_base.rstrip("/") + "/" + path.name,
        "sha256": digest.hexdigest(),
        "size": size,
    }


def normalise_release(r: object, what: str) -> dict:
    """A release entry checked against the caps, in the README's field order."""
    r = check_fields(r, RELEASE_FIELDS, ("min_from", "notes_url"), what)
    out = {"version": check_version(r["version"], f"{what} version")}
    out["date"] = check_int(r["date"], f"{what} date")
    if r["level"] not in LEVELS:
        raise Refused(f"{what} level must be one of {LEVELS}")
    out["level"] = r["level"]
    for key in ("security", "manual_steps"):
        out[key] = check_bool(r[key], f"{what} {key}")
    min_from = r.get("min_from")
    out["min_from"] = None if min_from is None else check_version(min_from, f"{what} min_from")
    out["withdrawn"] = check_bool(r["withdrawn"], f"{what} withdrawn")
    for key in ("migrates", "reindex"):
        out[key] = check_bool(r[key], f"{what} {key}")
    out["reindex_bytes"] = check_int(r["reindex_bytes"], f"{what} reindex_bytes")
    if not isinstance(r["notes"], str) or len(r["notes"].encode()) > MAX_NOTES:
        raise Refused(f"{what} notes must be text of at most {MAX_NOTES} bytes")
    out["notes"] = r["notes"]
    notes_url = r.get("notes_url")
    out["notes_url"] = None if notes_url is None else check_url(notes_url, None, f"{what} notes_url")
    arts = r["artifacts"]
    if not isinstance(arts, list) or len(arts) > MAX_ARTIFACTS:
        raise Refused(f"{what} must have at most {MAX_ARTIFACTS} artifacts")
    seen = set()
    out["artifacts"] = []
    for i, a in enumerate(arts):
        aw = f"{what} artifact {i}"
        a = check_fields(a, ARTIFACT_FIELDS, (), aw)
        if a["target"] not in TARGETS or a["kind"] not in KINDS:
            raise Refused(f"{aw}: target must be one of {TARGETS} and kind one of {KINDS}")
        if (a["target"], a["kind"]) in seen:
            raise Refused(f"{aw}: a second {a['kind']} for {a['target']}")
        seen.add((a["target"], a["kind"]))
        if not isinstance(a["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", a["sha256"]):
            raise Refused(f"{aw}: sha256 must be 64 lower-case hex digits")
        out["artifacts"].append({
            "target": a["target"],
            "kind": a["kind"],
            "url": check_url(a["url"], DOWNLOAD_HOSTS, f"{aw} url"),
            "sha256": a["sha256"],
            "size": check_int(a["size"], f"{aw} size", 1, MAX_ARTIFACT_SIZE),
        })
    return out


def load_statements(path: Path) -> list:
    """The key chain from `crates/update/keys/statements.json`, carried whole."""
    stmts = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(stmts, list) or len(stmts) > MAX_STATEMENTS:
        raise Refused(f"{path}: at most {MAX_STATEMENTS} statements")
    for i, s in enumerate(stmts):
        check_fields(s, ("body", "minisig", "cosig"), ("cosig",), f"statement {i}")
        if len(base64.b64decode(s["body"], validate=True)) > MAX_STATEMENT:
            raise Refused(f"statement {i} body is over {MAX_STATEMENT} bytes")
    return stmts


def statement_time(s: dict) -> int:
    body = json.loads(base64.b64decode(s["body"], validate=True))
    return check_int(body.get("timestamp"), "a statement's timestamp")


def load_metadata(path: Path | None, version: str) -> dict:
    if path is None:
        if "-" not in version:
            raise Refused(f"{version} is a release: its metadata file (level, security, manual_steps, "
                          "min_from, migrates, reindex, reindex_bytes) is required")
        print("::warning::No release metadata file: the manifest uses the conservative values "
              f"{json.dumps(CONSERVATIVE)}", file=sys.stderr)
        return dict(CONSERVATIVE)
    meta = check_fields(json.loads(path.read_text(encoding="utf-8")), tuple(CONSERVATIVE), ("min_from",), str(path))
    return {**CONSERVATIVE, **meta}


def build(a: argparse.Namespace) -> int:
    channel, version = a.channel, check_version(a.version)
    if channel == "stable" and "-" in version:
        raise Refused(f"{version} is a pre-release: it never goes on the stable channel")
    if channel == "nightly" and "-nightly." not in version:
        raise Refused(f"{version} isn't a nightly version")
    if not REPO.match(a.repo):
        raise Refused(f"--repo {a.repo!r} isn't OWNER/NAME")
    want = "nightly" if channel == "nightly" else f"v{version}"
    if a.tag != want:
        raise Refused(f"--tag must be {want!r} for {channel} {version}, not {a.tag!r}")
    downloads = f"https://github.com/{a.repo}/releases/download/"
    url_base = downloads + a.tag
    now = int(time.time()) if a.now is None else a.now

    previous: list = []
    timestamp = now
    if a.previous:
        prev = json.loads(Path(a.previous).read_bytes())
        check_fields(prev, ("type", "format", "channel", "timestamp", "expires", "statements", "releases"),
                     (), "the previous manifest")
        if prev["type"] != "lodestar-manifest" or prev["format"] != 1 or prev["channel"] != channel:
            raise Refused(f"the previous manifest isn't a format-1 {channel} manifest")
        if not isinstance(prev["releases"], list):
            raise Refused("the previous manifest's releases must be a list")
        previous = [normalise_release(r, f"previous release {i}") for i, r in enumerate(prev["releases"])]
        if any(r["version"] == version for r in previous):
            print(f"{version} is already in the published {channel} manifest; nothing to do.", file=sys.stderr)
            return EXIT_ALREADY
        # Clients refuse a manifest that isn't newer than the one they hold.
        timestamp = max(now, check_int(prev["timestamp"], "the previous timestamp") + 1)

    statements = load_statements(Path(a.statements))
    newest_statement = max((statement_time(s) for s in statements), default=0)
    if timestamp < newest_statement:
        raise Refused("the manifest would be dated before a key statement it carries")

    meta = load_metadata(Path(a.metadata) if a.metadata else None, version)
    notes = Path(a.notes_file).read_text(encoding="utf-8") if a.notes_file else ""
    files = [
        ("x86_64-unknown-linux-musl", "binary", a.x86_64_binary),
        ("aarch64-unknown-linux-musl", "binary", a.aarch64_binary),
        ("x86_64-pc-windows-msvc", "msi", a.msi),
        ("x86_64-pc-windows-msvc", "portable", a.portable),
    ]
    release = normalise_release({
        "version": version,
        "date": now,
        **meta,
        "withdrawn": False,
        "notes": notes.strip() or f"Lodestar {version}.",
        "notes_url": a.notes_url,
        "artifacts": [artifact(t, k, Path(p), url_base) for t, k, p in files],
    }, "this release")
    for r in [release, *previous]:
        under = url_base + "/" if r is release else downloads
        if any(not art["url"].startswith(under) for art in r["artifacts"]):
            raise Refused(f"{r['version']}: every artifact URL must be under {under}")

    releases = sorted([release, *previous], key=lambda r: version_key(r["version"]), reverse=True)
    keep = min(a.keep, MAX_RELEASES)
    if releases.index(release) >= keep:
        raise Refused(f"{version} is older than the newest {keep} releases already published")
    body = {
        "type": "lodestar-manifest",
        "format": 1,
        "channel": channel,
        "timestamp": timestamp,
        "expires": timestamp + MAX_LIFETIME,
        "statements": statements,
        "releases": releases[:keep],
    }
    out = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode()
    if len(out) > MAX_MANIFEST:
        raise Refused(f"the manifest body is {len(out)} bytes, over the {MAX_MANIFEST}-byte cap")
    Path(a.out).write_bytes(out)
    print(f"{channel} manifest: {version}, {len(body['releases'])} releases, {len(out)} bytes, "
          f"timestamp {timestamp}", file=sys.stderr)
    return 0


def extract(a: argparse.Namespace) -> int:
    """A published envelope's manifest body and signature, for minisign -V."""
    raw = Path(a.envelope).read_bytes()
    if len(raw) > MAX_ENVELOPE:
        raise Refused(f"the envelope is over {MAX_ENVELOPE} bytes")
    env = check_fields(json.loads(raw), ("manifest", "freshness"), ("freshness",), "the envelope")
    m = check_fields(env["manifest"], ("body", "minisig"), (), "the envelope's manifest")
    body = base64.b64decode(m["body"], validate=True)
    if len(body) > MAX_MANIFEST:
        raise Refused(f"the published manifest body is over {MAX_MANIFEST} bytes")
    Path(a.body_out).write_bytes(body)
    Path(a.minisig_out).write_text(m["minisig"], encoding="utf-8", newline="")
    return 0


def envelope(a: argparse.Namespace) -> int:
    """The `<channel>.json` envelope; the weekly re-sign adds `freshness`."""
    body = Path(a.body).read_bytes()
    if len(body) > MAX_MANIFEST:
        raise Refused(f"the manifest body is over {MAX_MANIFEST} bytes")
    minisig = Path(a.minisig).read_text(encoding="utf-8")
    if len(minisig.encode()) > 1024 or len(minisig.splitlines()) != 4:
        raise Refused("the signature file must be minisign's four lines, at most 1 KiB")
    env = {"manifest": {"body": base64.b64encode(body).decode(), "minisig": minisig}, "freshness": None}
    out = json.dumps(env, separators=(",", ":")).encode()
    if len(out) > MAX_ENVELOPE:
        raise Refused(f"the envelope is {len(out)} bytes, over the {MAX_ENVELOPE}-byte cap")
    Path(a.out).write_bytes(out)
    return 0


def names(a: argparse.Namespace) -> int:
    """The asset names a manifest body's artifact URLs end in, one per line."""
    body = json.loads(Path(a.body).read_bytes())
    for r in body["releases"]:
        for art in r["artifacts"]:
            print(art["url"].rsplit("/", 1)[1])
    return 0


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="command", required=True)
    b = sub.add_parser("build")
    b.add_argument("--channel", required=True, choices=CHANNELS)
    b.add_argument("--version", required=True)
    b.add_argument("--repo", required=True)
    b.add_argument("--tag", required=True)
    for name in ("--x86-64-binary", "--aarch64-binary", "--msi", "--portable", "--statements", "--out"):
        b.add_argument(name, required=True)
    b.add_argument("--previous")
    b.add_argument("--metadata")
    b.add_argument("--notes-file")
    b.add_argument("--notes-url")
    b.add_argument("--keep", type=int, default=MAX_RELEASES)
    b.add_argument("--now", type=int)
    b.set_defaults(run=build)
    e = sub.add_parser("extract")
    for name in ("envelope", "body_out", "minisig_out"):
        e.add_argument(name)
    e.set_defaults(run=extract)
    w = sub.add_parser("envelope")
    for name in ("body", "minisig", "out"):
        w.add_argument(name)
    w.set_defaults(run=envelope)
    n = sub.add_parser("names")
    n.add_argument("body")
    n.set_defaults(run=names)
    return p


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if getattr(args, "keep", 1) < 1:
        print("::error::--keep must be at least 1", file=sys.stderr)
        return 1
    try:
        return args.run(args)
    except (Refused, OSError, ValueError, KeyError, TypeError) as e:
        # ValueError covers bad JSON, base64 and UTF-8. None of these
        # inputs holds a key, so the reason is safe to print.
        print(f"::error::{e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
