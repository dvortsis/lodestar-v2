#!/usr/bin/env python3
"""Writes the seeds for the `irc_announce` fuzz target (X-irc-fuzz-seeds).

Run from the repository root:

    python3 fuzz/gen/irc_announce_seeds.py

The target's input is two bytes of definition length (big-endian), the
definition, then one server line. Each seed is an ordinary, valid or
near-valid input: the shipped template with announce lines, the target's
fixed definition (length 0, so the line is read against it alone) with
lines of its two shapes, and two definitions at their list caps.

Rewrites `fuzz/seeds/irc_announce/` whole, so a seed dropped here goes
from the folder too. Never hand-edit the seeds; change this file.
"""

import pathlib
import struct

ROOT = pathlib.Path(__file__).resolve().parents[2]
OUT = ROOT / "fuzz" / "seeds" / "irc_announce"
TEMPLATE = (ROOT / "crates/sources-irc/data/announce/template.def").read_text("utf-8")

# Every seed stays well under this.
MAX_SEED = 4096


def frame(definition: str, line: str) -> bytes:
    d = definition.encode("utf-8")
    return struct.pack(">H", len(d)) + d + line.encode("utf-8")


# The template's own announce shape, from its announcer to its channel.
TEMPLATE_HIT = (
    ":ExampleBot!bot@services.example-tracker.test PRIVMSG #example-announce "
    ":New Torrent: Some.Show.S01E02.1080p.WEB-DL Category: TV Size: 1.37 GiB"
    " - https://example-tracker.test/torrents/12345"
)


def caps_definition(case_variants: bool) -> str:
    """A definition with every list at its cap: 8 channels, 8 announcers,
    8 announcer hosts and 4 patterns. With `case_variants`, each list also
    repeats its entries in another case (IRC case mapping for channels and
    nicks, ASCII case for hosts), which the parser folds rather than
    counting past the cap."""
    lines = [
        "lodestar-announce = 1",
        "id = caps" + ("-folded" if case_variants else ""),
        "name = At the caps",
        "network = irc.caps.test",
    ]
    channels = [f"#announce-{i}" for i in range(7)] + ["#Caps[Main]"]
    announcers = [f"Bot{i}" for i in range(7)] + ["Caps[Bot]"]
    hosts = [f"bot{i}.caps.test" for i in range(8)]
    for c in channels:
        lines.append(f"channel = {c}")
    for a in announcers:
        lines.append(f"announcer = {a}")
    for h in hosts:
        lines.append(f"announcer-host = {h}")
    if case_variants:
        # RFC 1459 case mapping: `[]\~` are the upper case of `{}|^`.
        lines.append("channel = #caps{main}")
        lines.append("channel = #ANNOUNCE-0")
        lines.append("announcer = caps{bot}")
        lines.append("announcer = BOT1")
        lines.append("announcer-host = BOT0.CAPS.TEST")
    lines += [
        r"pattern = ^\[(?P<category>[^\]]+)\] (?P<name>\S+) \((?P<size>[0-9.]+ ?[KMGT]i?B)\) #(?P<id>[0-9]+)$",
        r"pattern = ^(?P<name>\S+) in (?P<category>\S+) #(?P<id>[0-9]+)$",
        r"pattern = ^(?P<name>\S+) (?P<size>[0-9]+) #(?P<id>[0-9]+)$",
        r"pattern = ^(?P<name>.+) #(?P<id>[A-Za-z0-9._-]+)$",
        "torrent-url = https://caps.test/get/{id}?key={passkey}&n={name}",
    ]
    return "\n".join(lines) + "\n"


SEEDS = {
    # The shipped template, framed, with a line that matches it.
    "template-announce": frame(TEMPLATE, TEMPLATE_HIT),
    # The same, as a NOTICE with a CRLF ending and mIRC bold and colour,
    # which the matcher strips before matching.
    "template-notice-formatted": frame(
        TEMPLATE,
        ":ExampleBot!bot@services.example-tracker.test NOTICE #EXAMPLE-ANNOUNCE "
        ":\x02New Torrent:\x02 \x0304,01Some.Film.2026.2160p\x03 Category: Movies"
        " Size: 58.2 GB - https://example-tracker.test/torrents/9\r",
    ),
    # Its announcer, its channel, but chatter the pattern doesn't match.
    "template-no-match": frame(
        TEMPLATE,
        ":ExampleBot!bot@services.example-tracker.test PRIVMSG #example-announce "
        ":Site maintenance tonight, announces may pause",
    ),
    # An announce-shaped line from a nick that isn't the announcer.
    "template-other-nick": frame(
        TEMPLATE, TEMPLATE_HIT.replace(":ExampleBot!bot@", ":SomeUser!user@")
    ),
    # The template saved with CRLF endings and a byte-order mark.
    "template-crlf-bom": frame(
        "﻿" + TEMPLATE.replace("\n", "\r\n"), TEMPLATE_HIT
    ),
    # No definition: the line is read against the target's fixed one.
    # Its first pattern, with a size.
    "fixed-with-size": frame(
        "",
        ":Announcer!a@announce.example.net PRIVMSG #announce "
        ":New: Some.Album.2026.FLAC [Music] 412.7 MiB id=a1b2c3",
    ),
    # Its first pattern, with a size in plain bytes.
    "fixed-size-bytes": frame(
        "",
        ":Announcer!a@announce.example.net PRIVMSG #announce "
        ":New: Some.Book.2026.EPUB [Books] 1048576 B id=77",
    ),
    # Its second pattern: no size, no category.
    "fixed-no-size": frame(
        "",
        ":Announcer!a@announce.example.net PRIVMSG #announce "
        ":Some.Release.2026.720p.HDTV id=4096",
    ),
    # Its second pattern without an id, which the fixed template's `{id}`
    # refuses (`Rejected(TorrentId)`).
    "fixed-no-id": frame(
        "", ":Announcer!a@announce.example.net PRIVMSG #announce :Some.Release.2026"
    ),
    # Message tags, as an IRCv3 server sends them, escapes included.
    "fixed-tags": frame(
        "",
        "@time=2026-11-27T08:00:00.000Z;msgid=abc123;+example.test/note=a\\sb\\:c "
        ":Announcer!a@announce.example.net PRIVMSG #announce "
        ":New: Some.Game.2026-GROUP [Games] 7,5 GB id=x_1.2",
    ),
    # A CTCP ACTION, which is never an announce.
    "fixed-action": frame(
        "",
        ":Announcer!a@announce.example.net PRIVMSG #announce "
        ":\x01ACTION New: Some.Release [TV] 1 GB id=1\x01",
    ),
    # Another command to the same channel.
    "fixed-topic": frame(
        "",
        ":Announcer!a@announce.example.net TOPIC #announce "
        ":New: Some.Release [TV] 1 GB id=1",
    ),
    # A server numeric, which has no nick to match.
    "fixed-numeric": frame(
        "", ":irc.example.net 332 me #announce :Announces only, no chat"
    ),
    # Every list at its cap, with a line from the last announcer and host
    # to the last channel, matching the first pattern.
    "caps": frame(
        caps_definition(False),
        ":Caps[Bot]!bot@bot7.caps.test PRIVMSG #Caps[Main] "
        ":[TV] Some.Show.S02E01.1080p (2.5 GiB) #31337",
    ),
    # The caps again with each list's entries repeated in another case,
    # and a line that reaches the fourth pattern through the folded names.
    "caps-folded": frame(
        caps_definition(True),
        ":caps{bot}!bot@BOT7.caps.test NOTICE #caps{main} "
        ":Some Film (2026) 2160p #rel-2026.1",
    ),
}


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for old in OUT.iterdir():
        old.unlink()
    for name, data in SEEDS.items():
        assert len(data) < MAX_SEED, f"{name} is {len(data)} bytes"
        (OUT / name).write_bytes(data)


if __name__ == "__main__":
    main()
