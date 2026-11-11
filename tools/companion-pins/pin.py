#!/usr/bin/env python3
"""Companion pin-refresh tool (CMP-13): pins a companion's builds and image.

For one companion and version it downloads each artifact from the entry's own
host allowlist (https, at most MAX_HOPS redirects, a byte cap, SHA-256 while
streaming), checks upstream's OpenPGP signature where upstream publishes one
(the runner's gpg, a temporary GNUPGHOME, keys pinned by full fingerprint in
upstream.py), resolves the container's digest, runs the pin-time checks, and
writes catalogue.json deterministically. Anything unexpected aborts with no
catalogue written (docs/decisions/2026-11-11-CMP-13.md).

Standard library only. Usage:
    pin.py --catalogue IN --out OUT --report REPORT.md --companion ID --version V
           [--tag T] [--image-tag T] [--artifacts linux-x86_64,...]
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

import upstream

# Mirrors crates/companions/src/catalogue.rs.
MAX_ARTIFACT_SIZE = 512 * 1024 * 1024
DOWNLOAD_HOSTS = ("github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com")
# Mirrors crates/lodestar/src/update/check.rs.
MAX_HOPS = 5
# Never fetched from, whatever an entry says (runner, 2026-11-11).
NEVER_HOSTS = ("sourceforge.net", "fosshub.com")
# Registry API hosts (and their token services) per catalogue registry.
REGISTRIES = {
    "lscr.io": ("lscr.io", ("lscr.io", "ghcr.io")),
    "ghcr.io": ("ghcr.io", ("ghcr.io",)),
    "docker.io": ("registry-1.docker.io", ("registry-1.docker.io", "auth.docker.io")),
}
SOURCE_HOSTS = ("codeload.github.com",)
# Where a registry redirects a blob (an image's config); read without credentials.
BLOB_HOSTS = {
    "lscr.io": ("pkg-containers.githubusercontent.com",),
    "ghcr.io": ("pkg-containers.githubusercontent.com",),
    "docker.io": ("production.cloudflare.docker.com",),
}
# Mirrors catalogue.rs's INSTALLER_SOURCES: an installer folder's host, never a fetch host.
INSTALLER_SOURCES = ("downloads.sourceforge.net",)
MAX_KEY = 1024 * 1024
MAX_SIG = 64 * 1024
MAX_MANIFEST = 4 * 1024 * 1024
MAX_TOKEN = 64 * 1024
MAX_SOURCE = 256 * 1024 * 1024
MAX_RUNTIMECONFIG = 64 * 1024
OVERALL = 30 * 60  # seconds for one download, every hop included
MANIFEST_TYPES = (
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
)
# OpenPGP hash algorithms accepted on a signature: SHA-256, SHA-384, SHA-512, SHA-224.
GOOD_HASHES = {"8", "9", "10", "11"}
VERSION = re.compile(r"^(0|[1-9][0-9]{0,8})(\.(0|[1-9][0-9]{0,8})){0,3}$")
# A tag or file name: URL-safe, and never `.`, `..` or a leading dot (HttpsUrl).
URL_PART = re.compile(r"^[A-Za-z0-9_~+-][A-Za-z0-9._~+-]*$")
MAX_SOURCE_FILE = 1024 * 1024
# What upstream text may be in the report as it is; anything else becomes `?`.
# `@` is out too: an `@name` would mention someone once the report is pasted.
_UNSAFE = re.compile(r"[^A-Za-z0-9 ._/+:=(),;!\"'-]")


def safe(text, limit: int = 200) -> str:
    """Upstream text for the report and messages: no Markdown, no code-fence breakout."""
    return _UNSAFE.sub("?", str(text))[:limit]


FINGERPRINT = re.compile(r"^([0-9A-F]{40}|[0-9A-F]{64})$")
KEYS = Path(__file__).resolve().parent / "keys"


class Refused(Exception):
    """A pin the tool won't make. `code` is stable; the message says why."""

    def __init__(self, code: str, message: str):
        super().__init__(f"{code}: {message}")
        self.code = code


# ─── Fetching ───────────────────────────────────────────────────────────────


class Response:
    def __init__(self, status: int, headers: dict[str, str], body):
        self.status = status
        self.headers = {k.lower(): v for k, v in headers.items()}
        self._body = body

    def chunks(self, size: int = 1 << 16, deadline: float | None = None):
        # No read may block past the deadline: the socket's own timeout is
        # cut to what's left before each read (a body with no socket, such
        # as the tests', has nothing to cut).
        sock = getattr(getattr(getattr(self._body, "fp", None), "raw", None), "_sock", None)
        while True:
            if deadline is not None and sock is not None:
                sock.settimeout(max(0.01, min(60.0, deadline - time.monotonic())))
            chunk = self._body.read(size)
            if not chunk:
                return
            yield chunk

    def close(self):
        self._body.close()


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None  # the caller follows redirects itself, through the allowlist


class Transport:
    """The network. Tests replace it with an in-memory one."""

    def __init__(self):
        self._opener = urllib.request.build_opener(_NoRedirect())

    def get(self, url: str, headers: dict[str, str]) -> Response:
        req = urllib.request.Request(url, headers={"User-Agent": "lodestar-companion-pins", **headers})
        try:
            r = self._opener.open(req, timeout=60)
        except urllib.error.HTTPError as e:  # 3xx and 4xx/5xx arrive here
            return Response(e.code, dict(e.headers), e)
        except (urllib.error.URLError, OSError) as e:
            raise Refused("network", f"{host_of(url)}: {e}") from None
        return Response(r.status, dict(r.headers), r)


def host_of(url: str) -> str:
    return urllib.parse.urlsplit(url).hostname or "?"


class PinTimeHosts:
    """Hosts read only when a pin is made, never by Lodestar: exact `names`,
    and one DNS label in front of `mirror_suffix` (SourceForge's download
    mirrors, `<mirror>.dl.sourceforge.net`). What's read there is trusted
    only through upstream's OpenPGP signature."""

    def __init__(self, names, mirror_suffix: str):
        self.names = frozenset(names)
        self.mirror_suffix = mirror_suffix

    def allows(self, host: str) -> bool:
        label = host.removesuffix(self.mirror_suffix)
        return host in self.names or (
            host.endswith(self.mirror_suffix)
            and re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label) is not None
        )


INSTALLER_HOSTS = PinTimeHosts(("downloads.sourceforge.net", "sourceforge.net"), ".dl.sourceforge.net")


def check_url(url: str, hosts) -> None:
    u = urllib.parse.urlsplit(url)
    host = (u.hostname or "").lower()
    pin_time = isinstance(hosts, PinTimeHosts)
    if not pin_time and any(host == n or host.endswith("." + n) for n in NEVER_HOSTS):
        raise Refused("host_not_allowed", f"{host} is never a fetch host")
    try:
        port = u.port
    except ValueError:
        port = -1
    allowed = hosts.allows(host) if pin_time else host in hosts
    if u.scheme != "https" or port is not None or u.username or u.password or not allowed:
        raise Refused("host_not_allowed", f"{u.scheme}://{host} is not on this fetch's allowlist")


def fetch(t: Transport, url: str, hosts, cap: int, sink, headers=None, missing_ok=False, seen=None) -> bool:
    """GETs `url`, following at most MAX_HOPS redirects, each on `hosts`, into
    `sink`, refusing a body over `cap`. A 404 on the first URL is False when
    `missing_ok`. Headers go to the first URL's host only. The final
    response's headers are copied into `seen`."""
    first = host_of(url)
    deadline = time.monotonic() + OVERALL
    for hop in range(MAX_HOPS + 1):
        check_url(url, hosts)
        sent = dict(headers or {}) if host_of(url) == first else {}
        r = t.get(url, sent)
        try:
            if r.status in (301, 302, 303, 307, 308):
                loc = r.headers.get("location")
                if not loc:
                    raise Refused("redirect", "a redirect without a Location")
                url = urllib.parse.urljoin(url, loc)
                continue
            if r.status == 404 and hop == 0 and missing_ok:
                return False
            if r.status != 200:
                raise Refused("http_status", f"{host_of(url)} answered {r.status}")
            if seen is not None:
                seen.update(r.headers)
            length = r.headers.get("content-length")
            if length is not None and (not length.isdigit() or int(length) > cap):
                raise Refused("too_large", f"{host_of(url)} sent a body over {cap} bytes")
            total = 0
            try:
                for chunk in r.chunks(deadline=deadline):
                    total += len(chunk)
                    if total > cap:
                        raise Refused("too_large", f"{host_of(url)} sent a body over {cap} bytes")
                    if time.monotonic() > deadline:
                        raise Refused("timeout", f"{host_of(url)} took over {OVERALL} s")
                    sink(chunk)
            except TimeoutError:
                raise Refused("timeout", f"{host_of(url)} took over {OVERALL} s, or stalled") from None
            return True
        finally:
            r.close()
    raise Refused("redirect", f"more than {MAX_HOPS} redirects")


def fetch_bytes(t, url, hosts, cap, headers=None, missing_ok=False) -> bytes | None:
    buf = bytearray()
    found = fetch(t, url, hosts, cap, buf.extend, headers, missing_ok)
    return bytes(buf) if found else None


def fetch_file(t, url, hosts, cap, path: Path) -> tuple[int, str]:
    """Streams `url` to `path`, hashing as it goes: (size, sha256 hex)."""
    h = hashlib.sha256()
    size = 0
    with open(path, "xb") as f:

        def sink(chunk):
            nonlocal size
            h.update(chunk)
            size += len(chunk)
            f.write(chunk)

        fetch(t, url, hosts, cap, sink)
    return size, h.hexdigest()


# ─── Upstream OpenPGP signatures ────────────────────────────────────────────


def gpg_run(gpg: str, home: str, args: list[str]) -> subprocess.CompletedProcess:
    cmd = [gpg, "--homedir", home, "--batch", "--no-tty", "--no-options", "--no-auto-key-retrieve", *args]
    return subprocess.run(
        cmd, capture_output=True, text=True, timeout=120, env={"PATH": os.environ.get("PATH", ""), "LC_ALL": "C"}
    )


def primaries(listing: str) -> list[str]:
    """Primary-key fingerprints in `gpg --with-colons` output."""
    out, after_pub = [], False
    for line in listing.splitlines():
        f = line.split(":")
        if f[0] in ("pub", "sec"):
            after_pub = True
        elif f[0] == "fpr" and after_pub:
            out.append(f[9])
            after_pub = False
        elif f[0] in ("sub", "ssb", "uid"):
            after_pub = False
    return out


def keys_for(t, gpg: str, policy, keys_dir: Path, work: Path) -> tuple[Path, list[str]]:
    """The folder of pinned key files to verify with: `keys/` when it holds
    every pinned key, else (for a policy that names its key sources) one
    key fetched now from each source. Each source must serve the pinned
    primary key; only that key, minimally exported, is used. The report
    carries it for committing as `keys/<FINGERPRINT>.asc`."""
    missing = [k for k in policy.keys if not (keys_dir / f"{k}.asc").is_file()]
    if not missing:
        return keys_dir, []
    if len(policy.keys) != 1 or len(policy.key_sources) < 2:
        raise Refused("key_not_pinned", f"no keys/{missing[0]}.asc, and no two sources to fetch it from")
    fpr = policy.keys[0]
    if not FINGERPRINT.match(fpr):
        raise Refused("key_not_pinned", "the policy names no full fingerprint for this artifact")
    exported = []
    for url, hosts in policy.key_sources:
        body = fetch_bytes(t, url, hosts, MAX_KEY)
        with tempfile.TemporaryDirectory(prefix="pins-keysrc-") as home:
            os.chmod(home, 0o700)
            src = Path(home) / "source.asc"
            src.write_bytes(body)
            gpg_run(gpg, home, ["--import", str(src)])
            if fpr not in primaries(gpg_run(gpg, home, ["--with-colons", "--list-keys"]).stdout):
                raise Refused("unknown_key", f"{host_of(url)} doesn't serve the pinned key {fpr}")
            out = gpg_run(gpg, home, ["--armor", "--export-options", "export-minimal", "--export", fpr])
            if out.returncode != 0 or "BEGIN PGP PUBLIC KEY BLOCK" not in out.stdout:
                raise Refused("unknown_key", f"the key from {host_of(url)} didn't export")
            exported.append(out.stdout)
    fetched = work / "fetched-keys"
    fetched.mkdir(exist_ok=True)
    (fetched / f"{fpr}.asc").write_text(exported[0], encoding="ascii")
    lines = [
        "",
        f"Key `{fpr}` wasn't in `keys/`: fetched from "
        + " and ".join(f"`{host_of(u)}`" for u, _ in policy.key_sources)
        + ", both serving that primary key. Commit it as `keys/"
        + fpr
        + ".asc` in the pin PR:",
        "",
        "```",
        *exported[0].strip().splitlines(),
        "```",
    ]
    return fetched, lines


def verify_openpgp(gpg: str, artifact: Path, sig: bytes, pinned: tuple[str, ...], keys_dir: Path) -> str:
    """Checks a detached signature on `artifact` with only the pinned keys in a
    fresh GNUPGHOME. Returns the signing key's primary fingerprint."""
    if not pinned or not all(FINGERPRINT.match(k) for k in pinned):
        raise Refused("key_not_pinned", "the policy names no full fingerprint for this artifact")
    with tempfile.TemporaryDirectory(prefix="pins-gnupg-") as home:
        os.chmod(home, 0o700)
        for fpr in pinned:
            keyfile = keys_dir / f"{fpr}.asc"
            if not keyfile.is_file():
                raise Refused("key_not_pinned", f"no keys/{fpr}.asc")
            shown = gpg_run(gpg, home, ["--with-colons", "--import-options", "show-only", "--import", str(keyfile)])
            if shown.returncode != 0 or primaries(shown.stdout) != [fpr]:
                raise Refused(
                    "unknown_key", f"keys/{fpr}.asc holds {primaries(shown.stdout) or 'no key'}, not exactly {fpr}"
                )
            if gpg_run(gpg, home, ["--import", str(keyfile)]).returncode != 0:
                raise Refused("unknown_key", f"keys/{fpr}.asc didn't import")
        listed = gpg_run(gpg, home, ["--with-colons", "--list-keys"])
        if sorted(primaries(listed.stdout)) != sorted(set(pinned)):
            raise Refused("unknown_key", "the keyring holds keys that aren't pinned")
        sigfile = Path(home) / "artifact.sig"
        sigfile.write_bytes(sig)
        # A detached signature holds signature packets only: a signed message
        # carries its own data, which gpg could verify instead of the artifact.
        packets = gpg_run(gpg, home, ["--list-packets", str(sigfile)])
        kinds = set(re.findall(r"^:(\w[\w ]*?) packet:", packets.stdout, re.M))
        if packets.returncode != 0 or kinds != {"signature"}:
            raise Refused("bad_signature", "the signature file isn't a detached signature")
        res = gpg_run(gpg, home, ["--status-fd", "1", "--verify", str(sigfile), str(artifact)])
        status = [line.split()[1:] for line in res.stdout.splitlines() if line.startswith("[GNUPG:] ")]
        words = [s[0] for s in status if s]
        # KEYEXPIRED and KEYREVOKED also report an unused subkey, so the
        # signature's own status words decide (gnupg doc/DETAILS).
        bad = {"BADSIG", "ERRSIG", "EXPSIG", "EXPKEYSIG", "REVKEYSIG", "NO_PUBKEY"}
        valid = [s for s in status if s and s[0] == "VALIDSIG"]
        if res.returncode != 0 or bad & set(words) or not valid:
            raise Refused(
                "bad_signature",
                f"gpg did not verify the signature ({', '.join(sorted(bad & set(words))) or 'no valid signature'})",
            )
        if words.count("NEWSIG") != len(valid) or words.count("GOODSIG") != len(valid):
            raise Refused("bad_signature", "not every signature in the file is a good one")
        signers = set()
        for v in valid:
            primary = v[10] if len(v) > 10 else v[1]
            if primary not in pinned:
                raise Refused("unknown_key", f"signed by {primary}, which isn't pinned")
            if len(v) < 10 or v[8] not in GOOD_HASHES:
                raise Refused("bad_signature", "the signature uses a weak hash")
            # Class 00, a binary document: a text-mode signature (01) also
            # verifies a file whose line endings differ.
            if v[9] != "00":
                raise Refused("bad_signature", f"signature class {safe(v[9])}, not a binary document (00)")
            signers.add(primary)
        if len(signers) != 1:
            raise Refused("bad_signature", "signed by more than one key")
        return signers.pop()


# ─── Pin-time checks on a build ─────────────────────────────────────────────


def read_member(path: Path, archive: str, name: str, cap: int, probe_only=False) -> bytes | None:
    """One regular file from a zip or tar.gz, at most `cap` bytes; None if
    absent. With `probe_only`, b"" if it's there, nothing read."""
    if archive == "zip":
        with zipfile.ZipFile(path) as z:
            names = z.namelist()
            if len(names) != len(set(names)):
                raise Refused("check_failed", "the archive holds two members of one name (CMP-2 refuses it)")
            try:
                info = z.getinfo(name)
            except KeyError:
                return None
            if probe_only and not info.is_dir():
                return b""
            if info.is_dir() or info.file_size > cap:
                raise Refused("check_failed", f"{name} is not a file of at most {cap} bytes")
            with z.open(info) as f:
                return f.read(cap + 1)
    with tarfile.open(path, "r:gz") as tf:
        members = tf.getmembers()
        names = [m.name.removeprefix("./") for m in members]
        if len(names) != len(set(names)):
            raise Refused("check_failed", "the archive holds two members of one name (CMP-2 refuses it)")
        for m in members:
            if m.name.removeprefix("./") == name:
                if probe_only and m.isreg():
                    return b""
                if not m.isreg() or m.size > cap:
                    raise Refused("check_failed", f"{name} is not a file of at most {cap} bytes")
                return tf.extractfile(m).read(cap + 1)
    return None


def dotnet_reads_socks5(text: bytes) -> str:
    """Mirrors manager::runtime_reads_proxy_env: .NET 8 or later in `tfm` and in
    every Microsoft.NETCore.App framework listed or included. A build listing
    only Microsoft.AspNetCore.App is refused."""
    try:
        opts = json.loads(text)["runtimeOptions"]
    except (ValueError, KeyError, TypeError):
        raise Refused("check_failed", "runtimeconfig.json doesn't read") from None

    def major(s):
        return int(s.split(".")[0]) if isinstance(s, str) and s.split(".")[0].isdigit() else None

    tfm = opts.get("tfm")
    tfm_major = major(tfm[3:]) if isinstance(tfm, str) and tfm.startswith("net") else None
    frameworks = []
    for key in ("framework", "frameworks", "includedFrameworks"):
        v = opts.get(key)
        frameworks += v if isinstance(v, list) else [v] if isinstance(v, dict) else []
    versions = [
        f.get("version") for f in frameworks if isinstance(f, dict) and f.get("name") == "Microsoft.NETCore.App"
    ]
    core = [major(v) for v in versions]
    if not core:
        raise Refused(
            "check_failed", "runtimeconfig.json names no Microsoft.NETCore.App (Lodestar refuses it on SOCKS5)"
        )
    if tfm_major is None or tfm_major < 8 or any(m is None or m < 8 for m in core):
        raise Refused("check_failed", f"{safe(tfm)} is older than .NET 8, whose proxy variables take socks5://")
    kind = "self-contained" if "includedFrameworks" in opts else "framework-dependent"
    return safe(f"{tfm}, {kind}, Microsoft.NETCore.App {', '.join(map(str, versions))}")


def check_dotnet_socks5(path: Path, artifact: dict) -> str:
    exe = artifact["executable"]
    stem = exe[:-4] if exe.lower().endswith(".exe") else exe
    if read_member(path, artifact["archive"], exe, 0, probe_only=True) is None:
        raise Refused("check_failed", f"{exe} isn't in the archive")
    text = read_member(path, artifact["archive"], f"{stem}.runtimeconfig.json", MAX_RUNTIMECONFIG)
    if text is None or len(text) > MAX_RUNTIMECONFIG:
        raise Refused(
            "check_failed", f"{stem}.runtimeconfig.json isn't beside the program (Lodestar refuses it on SOCKS5)"
        )
    return dotnet_reads_socks5(text)


def check_prowlarr_source(t: Transport, owner: str, project: str, tag: str, work: Path) -> list[str]:
    url = f"https://codeload.github.com/{owner}/{project}/tar.gz/refs/tags/{tag}"
    tarball = work / "source.tar.gz"
    fetch_file(t, url, SOURCE_HOSTS, MAX_SOURCE, tarball)
    counts = {k: [] for k in upstream.PROWLARR_SOURCE_LIMITS}
    defaults, dispatchers = 0, []
    dispatcher_counts = {k: 0 for k in upstream.PROWLARR_DISPATCHER_LIMITS}
    proxy_lines = []
    with tarfile.open(tarball, "r:gz") as tf:
        for m in tf:
            parts = m.name.split("/")
            if not m.isreg() or not m.name.endswith(".cs") or any("Test" in p.split(".") for p in parts):
                continue
            if m.size > MAX_SOURCE_FILE:
                raise Refused("check_failed", f"{safe(m.name)} is over {MAX_SOURCE_FILE} bytes")
            text = tf.extractfile(m).read(MAX_SOURCE_FILE + 1).decode("utf-8", "replace")
            rel = "/".join(parts[1:])
            for k, (pattern, _) in upstream.PROWLARR_SOURCE_LIMITS.items():
                counts[k] += [rel] * len(re.findall(pattern, text))
            if rel.endswith("Configuration/ConfigService.cs"):
                defaults += len(re.findall(upstream.PROWLARR_PROXY_DEFAULT, text))
            if rel.endswith("ManagedHttpDispatcher.cs"):
                dispatchers.append(rel)
                for k, (pattern, _) in upstream.PROWLARR_DISPATCHER_LIMITS.items():
                    dispatcher_counts[k] += len(re.findall(pattern, text))
                proxy_lines += [ln.strip() for ln in text.splitlines() if re.search(r"\bProxy\b", ln)]
    if defaults != 1:
        raise Refused("check_failed", "ConfigService.cs no longer defaults ProxyEnabled to false")
    if len(dispatchers) != 1:
        raise Refused("check_failed", f"expected one ManagedHttpDispatcher.cs, found {len(dispatchers)}")
    lines = ["ProxyEnabled defaults to false (ConfigService.cs)", f"dispatcher: `{safe(dispatchers[0])}`"]
    for k, (_, limit) in upstream.PROWLARR_DISPATCHER_LIMITS.items():
        if dispatcher_counts[k] > limit:
            raise Refused(
                "check_failed", f"`{k}` occurs {dispatcher_counts[k]} times in the dispatcher (at most {limit})"
            )
        lines.append(f"dispatcher `{k}`: {dispatcher_counts[k]} (at most {limit})")
    lines.append("dispatcher lines naming `Proxy`, for the pin PR's reviewer to read:")
    lines += [f"  - `{safe(ln)}`" for ln in proxy_lines[:20]]
    for k, (_, limit) in upstream.PROWLARR_SOURCE_LIMITS.items():
        found = counts[k]
        if len(found) > limit:
            where = ", ".join(safe(f) for f in sorted(set(found)))
            raise Refused("check_failed", f"`{k}` occurs {len(found)} times (at most {limit}): {where}")
        where = ", ".join(safe(f) for f in sorted(set(found)))
        lines.append(f"`{k}`: {len(found)} (at most {limit}){': `' + where + '`' if found else ''}")
    return lines


# ─── Container digests ──────────────────────────────────────────────────────


def bearer_params(header: str) -> dict[str, str]:
    if not header.lower().startswith("bearer "):
        raise Refused("registry", "the registry doesn't offer a bearer token")
    return dict(re.findall(r'(\w+)="([^"]*)"', header[7:]))


def probe(t: Transport, url: str, hosts, headers) -> tuple[str, str | None]:
    """Follows the manifest URL's redirects without credentials: the URL that
    answers, and its bearer challenge if it wants a token (sent to it alone)."""
    for _ in range(MAX_HOPS + 1):
        check_url(url, hosts)
        r = t.get(url, headers)
        r.close()
        if r.status in (301, 302, 303, 307, 308) and r.headers.get("location"):
            url = urllib.parse.urljoin(url, r.headers["location"])
            continue
        if r.status == 401:
            return url, r.headers.get("www-authenticate", "")
        if r.status == 200:
            return url, None
        raise Refused("http_status", f"{host_of(url)} answered {r.status}")
    raise Refused("redirect", f"more than {MAX_HOPS} redirects")


def resolve_digest(t: Transport, image: str, tag: str) -> tuple[str, list[str]]:
    """The digest of `image:tag`'s manifest (an index for multi-arch images),
    computed from the bytes received, and the platforms it lists."""
    registry, repo = image.split("/", 1)
    if registry not in REGISTRIES:
        raise Refused("host_not_allowed", f"{registry} isn't a catalogue registry")
    api, hosts = REGISTRIES[registry]
    if not URL_PART.match(tag) or tag == "latest":
        raise Refused("request", f"image tag {tag!r} isn't a fixed version tag")
    url = f"https://{api}/v2/{repo}/manifests/{tag}"
    accept = {"Accept": ", ".join(MANIFEST_TYPES)}
    url, challenge = probe(t, url, hosts, accept)
    headers = dict(accept)
    if challenge is not None:
        p = bearer_params(challenge)
        q = urllib.parse.urlencode({"service": p.get("service", ""), "scope": f"repository:{repo}:pull"})
        body = fetch_bytes(t, f"{p.get('realm', '')}?{q}", hosts, MAX_TOKEN)
        try:
            token = json.loads(body).get("token") or json.loads(body).get("access_token")
        except (ValueError, AttributeError):
            token = None
        if not isinstance(token, str) or not token:
            raise Refused("registry", "the token service sent no token")
        headers["Authorization"] = f"Bearer {token}"
    manifest, seen = bytearray(), {}
    fetch(t, url, hosts, MAX_MANIFEST, manifest.extend, headers, seen=seen)
    digest = "sha256:" + hashlib.sha256(manifest).hexdigest()
    claimed = seen.get("docker-content-digest")
    if claimed is not None and claimed != digest:
        raise Refused("digest_mismatch", f"the registry said {claimed}, the bytes are {digest}")
    try:
        doc = json.loads(manifest)
    except ValueError:
        raise Refused("registry", "the manifest isn't JSON") from None
    media = doc.get("mediaType") or seen.get("content-type", "").split(";")[0]
    if media not in MANIFEST_TYPES:
        raise Refused("registry", f"unexpected manifest type {safe(media)}")
    platforms = sorted(
        "/".join(filter(None, (m["platform"].get(k) for k in ("os", "architecture", "variant"))))
        for m in doc.get("manifests", [])
        if isinstance(m, dict) and isinstance(m.get("platform"), dict) and m["platform"].get("os") != "unknown"
    )
    if "manifests" not in doc:
        platforms = [single_platform(t, url, repo, doc, hosts, registry, headers)]
    if "linux/amd64" not in platforms:
        raise Refused("registry", f"the image has no linux/amd64 build ({safe(', '.join(platforms))})")
    return digest, platforms


def single_platform(t, url: str, repo: str, doc: dict, hosts, registry: str, headers: dict) -> str:
    """A plain manifest names no platform: its config blob does. The blob is
    fetched by its digest (the registry may redirect it to its blob host)
    and checked against that digest before it's read."""
    config = doc.get("config")
    digest = config.get("digest") if isinstance(config, dict) else None
    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise Refused("registry", "the manifest names no sha256 config")
    blob = bytearray()
    auth = {k: v for k, v in headers.items() if k == "Authorization"}
    blob_url = f"https://{host_of(url)}/v2/{repo}/blobs/{digest}"
    fetch(t, blob_url, (*hosts, *BLOB_HOSTS[registry]), MAX_MANIFEST, blob.extend, auth)
    if "sha256:" + hashlib.sha256(blob).hexdigest() != digest:
        raise Refused("digest_mismatch", "the image config isn't the one its manifest names")
    try:
        cfg = json.loads(blob)
        os_, arch = cfg["os"], cfg["architecture"]
    except (ValueError, KeyError, TypeError):
        raise Refused("registry", "the image config names no platform") from None
    return f"{safe(os_)}/{safe(arch)}"


# ─── The catalogue ──────────────────────────────────────────────────────────


def load_catalogue(text: str) -> dict:
    def no_duplicates(pairs):
        keys = [k for k, _ in pairs]
        if len(keys) != len(set(keys)):
            raise Refused("catalogue", f"duplicate key in {keys}")
        return dict(pairs)

    try:
        doc = json.loads(text, object_pairs_hook=no_duplicates)
    except ValueError as e:
        raise Refused("catalogue", f"catalogue.json doesn't read: {e}") from None
    if doc.get("format") != 1 or not isinstance(doc.get("entries"), list):
        raise Refused("catalogue", "not a format 1 catalogue")
    return doc


_E = ("entries", "*")
_EXPANDED = {(), ("entries",), _E, _E + ("licence",), _E + ("native",), _E + ("native", "artifacts"), _E + ("ports",)}
_EXPANDED |= {_E + ("native", "artifacts", "*", "pin")}


def _fmt(v, ind: int, path: tuple) -> str:
    expand = isinstance(v, (dict, list)) and bool(v)
    if path == _E + ("native", "artifacts", "*"):
        expand = expand and v.get("pin") is not None
    else:
        expand = expand and path in _EXPANDED
    if not expand:
        return json.dumps(v, ensure_ascii=False, separators=(", ", ": "))
    pad = " " * (ind + 2)
    if isinstance(v, dict):
        items = [f"{pad}{json.dumps(k)}: {_fmt(x, ind + 2, path + (k,))}" for k, x in v.items()]
        return "{\n" + ",\n".join(items) + "\n" + " " * ind + "}"
    items = [pad + _fmt(x, ind + 2, path + ("*",)) for x in v]
    return "[\n" + ",\n".join(items) + "\n" + " " * ind + "]"


def format_catalogue(doc: dict) -> str:
    """The catalogue in its committed layout: same input, same bytes."""
    return _fmt(doc, 0, ()) + "\n"


# ─── A pin ──────────────────────────────────────────────────────────────────


def loader_url_ok(url: str, release: str) -> bool:
    """`catalogue.rs`'s HttpsUrl and pin-URL rules: `<release><tag>/<file>`,
    a path of `[A-Za-z0-9._~/+-]`, no empty, `.` or `..` segment."""
    path = urllib.parse.urlsplit(url).path
    if not url.startswith("https://") or not re.fullmatch(r"/[A-Za-z0-9._~/+-]*", path):
        return False
    if any(seg in ("", ".", "..") for seg in path[1:].split("/")):
        return False
    rest = url.removeprefix(release)
    return rest != url and rest.count("/") == 1 and all(rest.split("/"))


def provenance() -> list[str]:
    """What made this pin: the commit, and the tool's and policy's own hashes."""
    here = Path(__file__).resolve().parent
    files = [here / "pin.py", here / "upstream.py", *sorted((here / "keys").glob("*.asc"))]
    lines = ["", "### Provenance", ""]
    for var in ("GITHUB_REPOSITORY", "GITHUB_REF", "GITHUB_SHA", "GITHUB_RUN_ID"):
        if os.environ.get(var):
            lines.append(f"- {var}: `{safe(os.environ[var])}`")
    lines += [f"- `{f.relative_to(here)}` SHA-256 `{hashlib.sha256(f.read_bytes()).hexdigest()}`" for f in files]
    return lines


def make_pins(args, doc: dict, t: Transport, gpg: str, keys_dir: Path, recipes=None) -> list[str]:
    """Pins `args.companion` in `doc` in place. Returns the report's lines."""
    recipes = upstream.RECIPES if recipes is None else recipes
    entry = next((e for e in doc["entries"] if e.get("id") == args.companion), None)
    recipe = recipes.get(args.companion)
    if entry is None or recipe is None:
        raise Refused("request", f"{args.companion!r} isn't a companion the tool can pin")
    if not VERSION.match(args.version):
        raise Refused("request", f"{args.version!r} isn't a version (1–4 numbers, no leading zeros)")

    def fill(s):
        return s.replace("{version}", args.version)

    report = [f"## {entry['name']} {args.version}", ""]
    native = entry.get("native")
    wanted = args.artifacts.split(",") if args.artifacts else None
    if native:
        tag = args.tag or (fill(recipe.tag) if recipe.tag else None)
        if not tag or not URL_PART.match(tag):
            raise Refused("request", "a release tag is needed (--tag), of URL-safe characters")
        if recipe.tag_pattern is not None:
            tag_ok = re.fullmatch(recipe.tag_pattern.replace("{version}", re.escape(args.version)), tag)
        else:
            tag_ok = recipe.tag is not None and tag == fill(recipe.tag)
        if not tag_ok:
            raise Refused("request", f"tag {safe(tag)} isn't the release of version {args.version}")
        release = native["release"]
        parts = urllib.parse.urlsplit(release).path.split("/")  # ['', owner, project, 'releases', 'download', '']
        hosts = tuple(h for h in entry["hosts"] if h in DOWNLOAD_HOSTS)
        keys = [f"{a['platform']}-{a['arch']}" for a in native["artifacts"]]
        for w in wanted or []:
            if w not in keys:
                raise Refused("request", f"{args.companion} has no {w} artifact")
        report += [f"Release tag `{tag}`, from `{release}`.", ""]
        report_keys: list[str] = []
        with tempfile.TemporaryDirectory(prefix="pins-") as work:
            work = Path(work)
            for a, key in zip(native["artifacts"], keys, strict=True):
                installer = a["archive"] == "installer"
                if installer and not recipe.installer_release:
                    if wanted and key in wanted:
                        raise Refused("installer", f"the tool has no installer source for {args.companion}")
                    report += [f"- **{key}** (installer): not pinned, awaiting its pin (CMP-11's guided install)."]
                    continue
                if wanted and key not in wanted:
                    continue
                if key not in recipe.files or key not in recipe.signature:
                    raise Refused("request", f"the tool has no file name or signature policy for {key}")
                name = fill(recipe.files[key])
                if not URL_PART.match(name):
                    raise Refused("request", f"{name!r} isn't a URL-safe file name")
                policy = recipe.signature[key]
                if installer:
                    # Its own folder, read only now (never by Lodestar), and
                    # trusted only through upstream's signature: a mirror
                    # proves nothing on its own.
                    if not isinstance(policy, upstream.OpenPgp):
                        raise Refused("installer", f"{key} is pinned only with upstream's OpenPGP signature")
                    folder = recipe.installer_release
                    folder_host = host_of(folder)
                    if folder_host not in INSTALLER_SOURCES or folder_host in entry["hosts"]:
                        raise Refused("host_not_allowed", f"{folder_host} isn't an installer source")
                    a_release, a_hosts, a_tag = folder, INSTALLER_HOSTS, fill(recipe.installer_dir or "")
                else:
                    a_release, a_hosts, a_tag = release, hosts, tag
                if not URL_PART.match(a_tag):
                    raise Refused("request", f"{safe(a_tag)} isn't a URL-safe folder")
                url = f"{a_release}{a_tag}/{name}"
                if not loader_url_ok(url, a_release):
                    raise Refused("request", f"{safe(url)} isn't a URL the catalogue accepts")
                path = work / key
                size, sha = fetch_file(t, url, a_hosts, MAX_ARTIFACT_SIZE, path)
                if size == 0:
                    raise Refused("check_failed", f"{key} is empty")
                if isinstance(policy, upstream.OpenPgp):
                    sig = fetch_bytes(t, url + policy.suffix, a_hosts, MAX_SIG, missing_ok=True)
                    if sig is None:
                        raise Refused(
                            "signature_missing", f"upstream signs {key}, but {name}{policy.suffix} isn't there"
                        )
                    used_keys, key_lines = keys_for(t, gpg, policy, keys_dir, work)
                    report_keys += key_lines
                    fpr = verify_openpgp(gpg, path, sig, policy.keys, used_keys)
                    upstream_sig = {"kind": "verified_at_pin", "fingerprint": fpr}
                    sig_line = f"OpenPGP signature verified, key `{fpr}`"
                else:
                    for suffix in (".asc", ".sig", ".gpg", ".minisig"):
                        if fetch_bytes(t, url + suffix, a_hosts, MAX_SIG, missing_ok=True) is not None:
                            raise Refused(
                                "signature_unexpected",
                                f"upstream now publishes {name}{suffix}: pin its key in upstream.py",
                            )
                    upstream_sig = {"kind": "none"}
                    sig_line = f"no upstream signature ({policy.why}; no .asc, .sig, .gpg or .minisig found)"
                checks = []
                if "dotnet_socks5" in recipe.checks:
                    checks.append(f".NET: {check_dotnet_socks5(path, a)}")
                new = {"version": args.version, "url": url, "size": size, "sha256": sha, "upstream_sig": upstream_sig}
                old = a.get("pin")
                if (
                    old
                    and old.get("version") == args.version
                    and (old.get("url"), old.get("size"), old.get("sha256"))
                    != (
                        url,
                        size,
                        sha,
                    )
                ):
                    # Upstream replaced a published file, or a host served
                    # other bytes: never re-pin one version silently.
                    raise Refused("sha_changed", f"{key} {args.version} was pinned as other bytes or another file")
                if installer:
                    # The folder sits before the pin, in the catalogue's layout.
                    pin_and_after = {k: a.pop(k) for k in list(a) if k in ("release", "pin")}
                    a["release"] = a_release
                    a["pin"] = pin_and_after.get("pin")
                a["pin"] = new
                report += [f"- **{key}** `{url}`", f"  - size {size}, SHA-256 `{sha}`", f"  - {sig_line}"]
                report += [f"  - {c}" for c in checks]
            if "prowlarr_source" in recipe.checks:
                report += ["", f"Prowlarr source at `{tag}`, outside its tests:"]
                report += [f"- {line}" for line in check_prowlarr_source(t, parts[1], parts[2], tag, work)]
        report += report_keys
        versions = {a["pin"]["version"] for a in native["artifacts"] if a.get("pin")}
        if len(versions) > 1:
            raise Refused(
                "request", f"the entry would hold pins of several versions {sorted(versions)}: pin every artifact"
            )
        if wanted:
            report += ["", f"Only {', '.join(wanted)} pinned; the other artifacts keep the pins they had."]
    elif wanted:
        raise Refused("request", f"{args.companion} has no native artifacts")
    container = entry.get("container")
    if container and args.skip_image:
        report += [
            "",
            "The image digest was not re-pinned: it may be another version's (the catalogue has no image version).",
        ]
    if container and not args.skip_image:
        image_tag = args.image_tag or (fill(recipe.image_tag) if recipe.image_tag else None)
        if not image_tag:
            raise Refused("request", "an image tag is needed (--image-tag)")
        digest, platforms = resolve_digest(t, container["image"], image_tag)
        container["digest"] = digest
        report += [
            *([""] if report[-1] else []),
            f"- **image** `{container['image']}:{image_tag}` → `{digest}`",
            f"  - platforms: `{safe(', '.join(platforms))}`"
            if platforms
            else "  - platforms: a single-platform manifest",
        ]
    return report


def main(argv=None, transport=None, keys_dir=KEYS) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--catalogue", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    p.add_argument("--report", type=Path)
    p.add_argument("--companion", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--tag")
    p.add_argument("--image-tag")
    p.add_argument(
        "--artifacts",
        help="comma-separated <platform>-<arch>; default: every one the recipe covers",
    )
    p.add_argument("--skip-image", action="store_true", help="leave the container digest as it is")
    p.add_argument("--gpg", default="gpg")
    args = p.parse_args(argv)
    staged = []
    try:
        doc = load_catalogue(args.catalogue.read_text(encoding="utf-8"))
        report = make_pins(args, doc, transport or Transport(), shutil.which(args.gpg) or args.gpg, keys_dir)
        text = format_catalogue(doc)
        load_catalogue(text)
        # Written only once every check passed. Both files are staged first,
        # then renamed, so a failed write never leaves a catalogue without
        # its report.
        outputs = [(args.out, text)]
        if args.report:
            outputs.append((args.report, "\n".join(["# Companion pin report", "", *report, *provenance(), ""])))
        for path, body in outputs:
            tmp = path.with_name(path.name + ".tmp")
            staged.append(tmp)
            tmp.write_text(body, encoding="utf-8")
        for (path, _), tmp in zip(outputs, staged, strict=True):
            os.replace(tmp, path)
    except Refused as e:
        print(f"companion-pins: refused, nothing written. {safe(e, 2000)}", file=sys.stderr)
        return 1
    except Exception as e:  # anything unforeseen fails closed, with a stable code too
        print(f"companion-pins: refused, nothing written. error: {type(e).__name__}: {safe(e)}", file=sys.stderr)
        return 1
    finally:
        for tmp in staged:
            tmp.unlink(missing_ok=True)
    print("\n".join(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
