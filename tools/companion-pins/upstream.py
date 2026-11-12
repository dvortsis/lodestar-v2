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
    """A detached OpenPGP signature at `<artifact URL><suffix>`, by one of `keys`.

    `key_sources` are where a pin run may fetch the one pinned key while
    `keys/<FINGERPRINT>.asc` isn't committed yet: (URL, hosts) pairs, each
    of which must serve that primary key, checked by fingerprint after
    import. They're for availability: the fingerprint here is the trust
    anchor (docs/decisions/2026-11-12-X-cmp13-installer-pins.md)."""

    suffix: str
    keys: tuple[str, ...]
    key_sources: tuple[tuple[str, tuple[str, ...]], ...] = ()


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
    # A regular expression a given --tag must match in full, "{version}"
    # standing for the version; None: a given tag must equal `tag`. It ties
    # the tag to the version the pins are labelled with.
    tag_pattern: str | None = None
    # An installer's own release folder (a catalogue.rs INSTALLER_SOURCES
    # host, read only at pin time) and its per-version folder in it.
    installer_release: str | None = None
    installer_dir: str | None = None
    # What ties the signed bytes to the version: the host's own folder feed
    # (size and MD5 per path) and the GitHub repository and annotated tag
    # whose date the signature mustn't predate.
    installer_feed: str | None = None
    installer_tag_repo: str | None = None
    installer_tag: str | None = None


USERDOCS = Unsigned(
    "userdocs/qbittorrent-nox-static publishes the static binaries and build-info JSON as "
    "GitHub release assets, with no .asc or .sig beside them"
)
# qBittorrent's release manager, sledgehammer999, signs the release tags
# on GitHub and the files on SourceForge with this key
# (docs/decisions/2026-11-12-X-cmp13-installer-pins.md).
QBITTORRENT_KEY = "D8F3DA77AAC6741053599C136E4A2D025B7CC9A2"
QBITTORRENT_INSTALLER = OpenPgp(
    ".asc",
    (QBITTORRENT_KEY,),
    key_sources=(
        (
            f"https://keyserver.ubuntu.com/pks/lookup?op=get&options=mr&search=0x{QBITTORRENT_KEY}",
            ("keyserver.ubuntu.com",),
        ),
        ("https://github.com/sledgehammer999.gpg", ("github.com",)),
    ),
)
PROWLARR = Unsigned("Prowlarr publishes tar.gz and zip builds as GitHub release assets, with no signature files")

RECIPES: dict[str, Recipe] = {
    # The Windows installer (CMP-11) is official only on SourceForge and
    # FossHub, never fetch hosts. It's read from SourceForge at pin time only
    # and pinned only with the release key's signature on it.
    "qbittorrent": Recipe(
        tag=None,  # "release-<qBittorrent>_v<libtorrent>": give --tag
        files={
            "linux-x86_64": "x86_64-qbittorrent-nox",
            "linux-aarch64": "aarch64-qbittorrent-nox",
            # The libtorrent 2.0 build; SourceForge also has an `_lt12_` one.
            "windows-x86_64": "qbittorrent_{version}_x64_setup.exe",
        },
        signature={"linux-x86_64": USERDOCS, "linux-aarch64": USERDOCS, "windows-x86_64": QBITTORRENT_INSTALLER},
        installer_release="https://downloads.sourceforge.net/project/qbittorrent/qbittorrent-win32/",
        installer_dir="qbittorrent-{version}",
        installer_feed="https://sourceforge.net/projects/qbittorrent/rss?path=/qbittorrent-win32/qbittorrent-{version}",
        installer_tag_repo="qbittorrent/qBittorrent",
        installer_tag="release-{version}",
        image_tag="{version}",
        tag_pattern=r"release-{version}_v[0-9]+(\.[0-9]+){1,3}",
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
    # The dispatcher's one client; any other client, handler or process-wide
    # proxy could take traffic past DefaultProxy.
    "new HttpClient(": (r"\bnew\s+(System\.Net\.Http\.)?HttpClient\s*\(", 1),
    "new HttpClientHandler": (r"\bnew\s+(System\.Net\.Http\.)?HttpClientHandler\b", 0),
    "DefaultProxy =": (r"\bDefaultProxy\s*=(?!=)", 0),
    "DefaultWebProxy =": (r"\bDefaultWebProxy\s*=(?!=)", 0),
}
# In ManagedHttpDispatcher.cs: the one `Proxy =` (set only when Prowlarr's own
# proxy is configured), and no `UseProxy` assignment at all, so with its own
# proxy off the handler keeps UseProxy's default and .NET's DefaultProxy.
# The report lists the dispatcher's `Proxy` lines for the pin PR's reviewer,
# who confirms the assignment is still behind that check.
PROWLARR_DISPATCHER_LIMITS: dict[str, tuple[str, int]] = {
    "Proxy =": (r"\bProxy\s*=(?!=)", 1),
    "UseProxy =": (r"\bUseProxy\s*=(?!=)", 0),
}
# `ProxyEnabled` must default to false, so its own proxy is off at the first
# start and .NET's DefaultProxy (the launch environment's route) applies.
PROWLARR_PROXY_DEFAULT = r'\bProxyEnabled\s*=>\s*GetValueBoolean\(\s*"ProxyEnabled"\s*,\s*false\s*\)'
