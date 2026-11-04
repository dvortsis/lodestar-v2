"""Writes the cases in crates/sources-plugins/fixtures/clients/: hand-written
in each client's API shapes, with real .torrent files whose info-hashes are
the listed keys. Run from the repository root:

    python3 -I crates/sources-plugins/fixtures/clients/make_fixtures.py .
"""
import hashlib, json, os, shutil, sys

root = os.path.join(sys.argv[1], "crates/sources-plugins/fixtures/clients")

def benc(v):
    if isinstance(v, int): return b"i%de" % v
    if isinstance(v, str): v = v.encode()
    if isinstance(v, bytes): return b"%d:%s" % (len(v), v)
    if isinstance(v, list): return b"l" + b"".join(benc(x) for x in v) + b"e"
    if isinstance(v, dict):
        return b"d" + b"".join(benc(k) + benc(v[k]) for k in sorted(v)) + b"e"
    raise TypeError(v)

def torrent(info):
    raw = benc(info)
    # A private tracker's announce URL with a passkey: never kept.
    t = b"d8:announce" + benc("http://tracker.example/REDACTED/announce") + b"4:info" + raw + b"e"
    return t, hashlib.sha1(raw).hexdigest()

def pieces(n): return bytes(range(20)) * n

bunny, H1 = torrent({"name": "Big Buck Bunny (2008)", "piece length": 262144,
    "files": [{"length": 524288, "path": ["Big Buck Bunny.mkv"]},
              {"length": 1024, "path": ["Subs", "en.srt"]}],
    "pieces": pieces(3), "private": 1})
# A valid torrent, but not the one listed as H3: its info dict is unused.
other, _ = torrent({"name": "Elephants Dream", "length": 1000, "piece length": 16384,
    "pieces": pieces(1)})
H2 = "00112233445566778899aabbccddeeff00112233"
H3 = "abcdefabcdefabcdefabcdefabcdefabcdef0123"
H4 = "0123456789abcdef0123456789abcdef01234567"

def case(name, exchanges):
    d = os.path.join(root, name)
    os.makedirs(d)
    for i, (req, resp, body, req_body) in enumerate(exchanges, 1):
        base = os.path.join(d, "%03d" % i)
        open(base + ".http", "w").write(req + "\n\n" + resp + "\n")
        if body is not None:
            open(base + ".body", "wb").write(body if isinstance(body, bytes) else body.encode())
        if req_body is not None:
            open(base + ".req.body", "wb").write(req_body.encode())

for old in ("qbittorrent_poll", "qbittorrent_refused", "transmission_poll", "deluge_poll"):
    shutil.rmtree(os.path.join(root, old), ignore_errors=True)
J = lambda v: json.dumps(v, separators=(",", ":"), sort_keys=True)

# qBittorrent 4.6 (API 2.9.3).
Q = "http://qbittorrent.example:8080"
QH = "Referer: %s/\nOrigin: %s" % (Q, Q)
login = ("POST %s/api/v2/auth/login\n%s\nContent-Type: application/x-www-form-urlencoded" % (Q, QH),
         "HTTP 200\nSet-Cookie: SID=REDACTED; HttpOnly; path=/; SameSite=Strict",
         "Ok.", "username=REDACTED&password=REDACTED")
def qinfo(rows):
    return ("GET %s/api/v2/torrents/info\n%s\nCookie: SID=REDACTED" % (Q, QH),
            "HTTP 200\nContent-Type: application/json", J(rows), None)
def qexport(h, status, body):
    return ("GET %s/api/v2/torrents/export?hash=%s\n%s\nCookie: SID=REDACTED" % (Q, h, QH),
            "HTTP %d\nContent-Type: application/x-bittorrent" % status, body, None)
row1 = {"hash": H1, "name": "Big Buck Bunny (2008)", "size": 525312, "total_size": 525312,
        "state": "uploading", "category": "", "tracker": "http://tracker.example/REDACTED/announce"}
row2 = {"hash": H2, "name": "Sintel (2010)", "size": -1, "total_size": -1, "state": "metaDL"}
row3 = {"hash": H3, "name": "Elephants Dream (2006) 720p", "total_size": 815000000, "state": "pausedDL"}
case("qbittorrent_poll", [
    login,
    qinfo([row1, row2, {"hash": "not-a-hash"}, 42]),
    qexport(H1, 200, bunny),
    # No metadata yet: qBittorrent answers 409.
    ("GET %s/api/v2/torrents/export?hash=%s\n%s\nCookie: SID=REDACTED" % (Q, H2, QH),
     "HTTP 409\nContent-Type: text/plain", "Torrent metadata hasn't been downloaded yet", None),
    qinfo([row1, row2, row3]),
    qexport(H3, 200, other),
])
case("qbittorrent_refused", [
    (login[0], "HTTP 200\nContent-Type: text/plain", "Fails.", login[3]),
])

# Transmission 4.0 (RPC 17).
T = "http://transmission.example:9091/transmission/rpc"
auth = "Authorization: Basic UkVEQUNURUQ6UkVEQUNURUQ="
tget = J({"method": "torrent-get", "arguments": {"fields": ["hashString", "name", "totalSize", "status", "error"]}})
def tlist(rows):
    return J({"arguments": {"torrents": rows}, "result": "success"})
t1 = {"hashString": H1, "name": "Big Buck Bunny (2008)", "totalSize": 525312, "status": 6, "error": 0}
t2 = {"hashString": H2, "name": "Sintel (2010)", "totalSize": 0, "status": 0, "error": 0}
t3 = {"hashString": H4, "name": "Tears of Steel (2012)", "totalSize": 6000000000, "status": 4, "error": 0}
case("transmission_poll", [
    ("POST %s\nContent-Type: application/json\n%s" % (T, auth),
     "HTTP 409\nX-Transmission-Session-Id: REDACTED", "<h1>409: Conflict</h1>", tget),
    ("POST %s\nContent-Type: application/json\n%s\nX-Transmission-Session-Id: REDACTED" % (T, auth),
     "HTTP 200\nContent-Type: application/json", tlist([t1, t2, {"hashString": 5}]), tget),
    ("POST %s\nContent-Type: application/json\n%s\nX-Transmission-Session-Id: REDACTED" % (T, auth),
     "HTTP 200\nContent-Type: application/json", tlist([t1, t2, t3]), tget),
])

# Deluge 2.1 Web UI.
D = "http://deluge.example:8112/json"
def dreq(method, params, id, cookie=True):
    head = "POST %s\nContent-Type: application/json\nAccept: application/json" % D
    if cookie: head += "\nCookie: _session_id=REDACTED"
    return head, J({"method": method, "params": params, "id": id})
def dex(method, params, id, result, error=None, cookie=True, set_cookie=False):
    head, body = dreq(method, params, id, cookie)
    resp = "HTTP 200\nContent-Type: application/json"
    if set_cookie: resp += "\nSet-Cookie: _session_id=REDACTED; Path=/json"
    return (head, resp, J({"id": id, "result": result, "error": error}), body)
status = [{}, ["name", "total_size", "state"]]
d1 = {H1: {"name": "Big Buck Bunny (2008)", "total_size": 525312, "state": "Seeding"},
      H2: {"name": "Sintel (2010)", "total_size": 0, "state": "Paused"},
      "short": {"name": "x"}}
d2 = dict(d1); del d2["short"]; d2[H4] = {"name": "Tears of Steel (2012)", "total_size": 6000000000, "state": "Downloading"}
case("deluge_poll", [
    dex("auth.login", ["REDACTED"], 1, True, cookie=False, set_cookie=True),
    dex("web.connected", [], 2, True),
    dex("core.get_torrents_status", status, 3, d1),
    # The session expired between polls: one new login, one retry.
    dex("core.get_torrents_status", status, 4, None, {"code": 1, "message": "Not authenticated"}),
    dex("auth.login", ["REDACTED"], 5, True, cookie=False, set_cookie=True),
    dex("web.connected", [], 6, True),
    dex("core.get_torrents_status", status, 7, d2),
])
print(H1)
