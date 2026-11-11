#!/usr/bin/env python3
"""Shared steps of the packaged-artifact smoke: setup -> crawling -> import -> search.

Standard library only, so the same file runs on Linux and Windows runners. Every
subcommand talks to a Lodestar on `--url` (default http://127.0.0.1:8090).

  flow.py fixture  OUT.sqlite3               a tiny Magnetico database (3 torrents)
  flow.py setup    --log FILE --state DIR    complete first-run setup with the logged token
  flow.py crawler  --state DIR [--dht]       wait until the crawler has left the setup hold
  flow.py search   --state DIR               wait until the fixture is searchable

Nothing here fetches anything outside the Lodestar under test. The setup token is
read from the server's own log and is only ever printed redacted; the password is
random, kept in `--state` (a temp folder the caller deletes) and never printed.
Every wait is a loop on a condition with a deadline of at least 10 s: no fixed sleeps.
"""
import argparse
import json
import os
import re
import secrets
import sqlite3
import sys
import time
import urllib.error
import urllib.request

API = "/api/v1"
# The fixture's names, and the word the search looks for. One word the three share.
WORD = "smokefixture"
NAMES = [
    "Lodestar smokefixture Alpha 1080p",
    "Lodestar smokefixture Beta 2160p",
    "Lodestar smokefixture Gamma 720p",
]
# Waits (seconds). Generous on purpose: the slowest runner is a Windows one.
SETUP_WAIT = 60
CRAWLER_WAIT = 60
SEARCH_WAIT = 90
TOKEN = re.compile(r"setup token\s+([0-9A-Fa-f]{4}(?:-[0-9A-Fa-f]{4}){3})")


def fail(msg):
    print(f"::error::{msg}", file=sys.stderr)
    sys.exit(1)


def wait_for(what, deadline_s, probe):
    """Calls `probe` until it returns something truthy; fails at the deadline."""
    end = time.monotonic() + max(deadline_s, 10)
    last = None
    while True:
        try:
            got = probe()
            if got:
                return got
            last = None
        except (urllib.error.URLError, OSError, ValueError) as e:
            last = e
        if time.monotonic() >= end:
            fail(f"timed out waiting for {what}" + (f" ({last})" if last else ""))
        time.sleep(0.5)


def request(url, path, *, body=None, cookie=None, timeout=15):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url.rstrip("/") + path, data=data)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if cookie:
        req.add_header("Cookie", cookie)
    return urllib.request.urlopen(req, timeout=timeout)


def get_json(url, path, cookie=None):
    with request(url, path, cookie=cookie) as r:
        return json.load(r)


def cookie_file(state):
    return os.path.join(state, "session")


def read_cookie(state):
    try:
        with open(cookie_file(state), encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        fail("no session: run `setup` first")


def cmd_fixture(args):
    out = args.out
    if os.path.exists(out):
        os.remove(out)
    db = sqlite3.connect(out)
    db.executescript(
        """
        CREATE TABLE torrents (
            id INTEGER PRIMARY KEY,
            info_hash BLOB NOT NULL UNIQUE,
            name TEXT NOT NULL,
            total_size INTEGER NOT NULL CHECK(total_size > 0),
            discovered_on INTEGER NOT NULL CHECK(discovered_on > 0)
        );
        CREATE TABLE files (
            id INTEGER PRIMARY KEY,
            torrent_id INTEGER REFERENCES torrents ON DELETE CASCADE ON UPDATE RESTRICT,
            size INTEGER NOT NULL,
            path TEXT NOT NULL
        );
        CREATE UNIQUE INDEX info_hash_index ON torrents (info_hash);
        CREATE INDEX files_torrent_id_index ON files (torrent_id);
        PRAGMA user_version = 1;
        """
    )
    for i, name in enumerate(NAMES, 1):
        h = bytes([0xA0 + i]) * 20
        db.execute(
            "INSERT INTO torrents (id, info_hash, name, total_size, discovered_on) VALUES (?,?,?,?,?)",
            (i, h, name, 1_000_000_000 * i, 1_700_000_000 + i),
        )
        db.execute(
            "INSERT INTO files (torrent_id, size, path) VALUES (?,?,?)",
            (i, 1_000_000_000 * i, f"{name}/{name}.mkv"),
        )
    db.commit()
    # No -wal sidecar: the importer wants a database it can open on its own.
    db.execute("PRAGMA journal_mode = DELETE")
    db.close()
    print(f"fixture: {len(NAMES)} torrents in {os.path.basename(out)}")


def cmd_setup(args):
    def token():
        for log in args.log:
            try:
                with open(log, encoding="utf-8", errors="replace") as f:
                    m = TOKEN.search(f.read())
            except OSError:
                continue
            if m:
                return m.group(1)
        return None

    tok = wait_for("the setup token in the log", SETUP_WAIT, token)
    print("setup token found: XXXX-XXXX-XXXX-XXXX (redacted)")
    notice = get_json(args.url, API + "/legal/notice")["version"]
    password = secrets.token_urlsafe(18)
    try:
        with request(
            args.url,
            API + "/setup",
            body={"token": tok, "password": password, "notice_version": notice},
        ) as r:
            raw = r.headers.get("Set-Cookie", "")
    except urllib.error.HTTPError as e:
        fail(f"POST /setup answered {e.code}")
    m = re.match(r"([^=;\s]+=[^;\s]+)", raw)
    if not m:
        fail("setup answered without a session cookie")
    os.makedirs(args.state, exist_ok=True)
    path = cookie_file(args.state)
    with open(path, "w", encoding="utf-8") as f:
        f.write(m.group(1))
    try:
        os.chmod(path, 0o600)
    except OSError:
        pass
    # The password is not needed again by the smoke; keep nothing to leak.
    del password
    print(f"setup done: accepted Legal Notice version {notice}, signed in")


def cmd_crawler(args):
    cookie = read_cookie(args.state)

    def running():
        return get_json(args.url, API + "/stats", cookie).get("paused") is False

    wait_for("the crawler to leave the setup hold (stats.paused false)", CRAWLER_WAIT, running)
    print("crawler: out of the setup hold (stats.paused = false)")
    if args.dht:

        def bound():
            with request(args.url, "/metrics") as r:
                text = r.read().decode("utf-8", "replace")
            return 'lodestar_source_up{source="dht"} 1' in text

        wait_for("the DHT source to be up (socket bound)", CRAWLER_WAIT, bound)
        print('crawler: lodestar_source_up{source="dht"} = 1')


def cmd_search(args):
    cookie = read_cookie(args.state)

    def found():
        r = get_json(args.url, API + f"/search?q={WORD}&limit=20", cookie)
        return r["hits"] if len(r["hits"]) >= len(NAMES) else None

    hits = wait_for(f"a search for {WORD!r} to find all {len(NAMES)} fixture torrents", SEARCH_WAIT, found)
    print(f"search: {len(hits)} hits for {WORD!r}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(s):
        s.add_argument("--url", default="http://127.0.0.1:8090")
        s.add_argument("--state", required=True, help="a temp folder for the session (deleted by the caller)")

    s = sub.add_parser("fixture")
    s.add_argument("out")
    s.set_defaults(fn=cmd_fixture)
    s = sub.add_parser("setup")
    common(s)
    s.add_argument(
        "--log", required=True, action="append", help="the server's log (repeat for stdout and stderr files)"
    )
    s.set_defaults(fn=cmd_setup)
    s = sub.add_parser("crawler")
    common(s)
    s.add_argument("--dht", action="store_true", help="also require the DHT source to be up")
    s.set_defaults(fn=cmd_crawler)
    s = sub.add_parser("search")
    common(s)
    s.set_defaults(fn=cmd_search)
    args = p.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
