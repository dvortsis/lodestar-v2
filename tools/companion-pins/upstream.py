"""What each companion's upstream publishes, and how its pin is made (CMP-13).

This file is the tool's trust policy. Changing it is a code change with its
own review, like `DOWNLOAD_HOSTS` in `crates/companions/src/catalogue.rs`.

- `tag` and `files` turn a version into the release tag and each artifact's
  file name in the entry's `native.release` folder.
- `signature` is per artifact: `None` where upstream signs nothing (the SHA-256
  pin is then the only gate), or an `OpenPgp` naming the signature file's
  suffix and the upstream keys, pinned by full fingerprint. Each key's public
  half is committed as `keys/<FINGERPRINT>.asc`. No key is pinned today: no
  artifact the catalogue can hold is signed upstream (see
  `docs/decisions/2026-11-11-CMP-13.md`).
- `image_tag` is the registry tag whose digest is pinned for the container.
- `checks` are the pin-time checks run on the downloaded build.
"""

from dataclasses import dataclass


@dataclass(frozen=True)
class OpenPgp:
    """A detached OpenPGP signature at `<artifact URL><suffix>`, by one of `keys`."""

    suffix: str
    keys: tuple[str, ...]


@dataclass(frozen=True)
class Unsigned:
    """Upstream publishes no signature for this artifact; `why` says how we know."""

    why: str


@dataclass(frozen=True)
class Recipe:
    tag: str | None  # e.g. "v{version}"; None: the tag must be given
    files: dict[str, str]  # "<platform>-<arch>" -> file name, "{version}" filled in
    signature: dict[str, OpenPgp | Unsigned]  # "<platform>-<arch>" -> what upstream signs
    image_tag: str | None  # e.g. "{version}"; None: the image tag must be given
    checks: tuple[str, ...] = ()


USERDOCS = Unsigned(
    "userdocs/qbittorrent-nox-static publishes the static binaries and build-info JSON as "
    "GitHub release assets, with no .asc or .sig beside them"
)
PROWLARR = Unsigned("Prowlarr publishes tar.gz and zip builds as GitHub release assets, with no signature files")

RECIPES: dict[str, Recipe] = {
    # The Windows installer (CMP-11) is not here: the official one is on
    # SourceForge and FossHub only, never a fetch host, and the catalogue
    # format has no per-artifact release folder for it yet. The tool refuses it.
    "qbittorrent": Recipe(
        tag=None,  # "release-<qBittorrent>_v<libtorrent>": give --tag
        files={
            "linux-x86_64": "x86_64-qbittorrent-nox",
            "linux-aarch64": "aarch64-qbittorrent-nox",
        },
        signature={"linux-x86_64": USERDOCS, "linux-aarch64": USERDOCS},
        image_tag="{version}",
    ),
    "prowlarr": Recipe(
        tag="v{version}",
        files={
            "linux-x86_64": "Prowlarr.master.{version}.linux-core-x64.tar.gz",
            "linux-aarch64": "Prowlarr.master.{version}.linux-core-arm64.tar.gz",
            "windows-x86_64": "Prowlarr.master.{version}.windows-core-x64.zip",
        },
        signature={"linux-x86_64": PROWLARR, "linux-aarch64": PROWLARR, "windows-x86_64": PROWLARR},
        image_tag="{version}",
        checks=("dotnet_socks5", "prowlarr_source"),
    ),
    "gluetun": Recipe(tag=None, files={}, signature={}, image_tag="v{version}"),
}

# Prowlarr's source at the pinned tag, searched outside its test projects
# (docs/decisions/2026-11-11-X-cmp-egress-prelaunch.md). Each pattern may occur
# at most this many times; more fails the pin until someone re-reads the new
# code and raises the number in a reviewed PR.
PROWLARR_SOURCE_LIMITS: dict[str, tuple[str, int]] = {
    "TcpClient": (r"\bTcpClient\b", 0),
    "UdpClient": (r"\bUdpClient\b", 0),
    "WebRequest.Create": (r"\bWebRequest\.Create\b", 0),
    "HttpWebRequest": (r"\bHttpWebRequest\b", 0),
    "UseProxy = false": (r"\bUseProxy\s*=\s*false\b", 0),
    "new SocketsHttpHandler": (r"\bnew\s+SocketsHttpHandler\b", 1),
    "new Socket(": (r"\bnew\s+Socket\s*\(", 1),
    "Dns.*": (r"\bDns\.[A-Z]\w*", 6),
}
# `ProxyEnabled` must default to false, so its own proxy is off at the first
# start and .NET's DefaultProxy (the launch environment's route) applies.
PROWLARR_PROXY_DEFAULT = r'\bProxyEnabled\s*=>\s*GetValueBoolean\(\s*"ProxyEnabled"\s*,\s*false\s*\)'
