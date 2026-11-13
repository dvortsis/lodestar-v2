#!/usr/bin/env python3
"""G-import-smoke stage driver: runs the offline import ladder for one stage
and measures build-plan §0.6's capacity list. Prints one JSON object of
aggregates (the only output the report uses; raw logs stay in WORK).

Usage:
  smoke.py --bin target/release/lodestar --source m10k.sqlite3 \
           --overlap o10k.sqlite3 --work /path/to/empty/dir [--load-secs 20]
  smoke.py ... --kind bitmagnet --pg-host DIR --pg-password-file DIR/reader_pw
           (after pg_bitmagnet.sh up m10k.sqlite3 DIR; --source still names
           the Magnetico file, for the hashes and the query mix)

WORK must not exist (or be empty). The driver runs the server on
127.0.0.1:18090 with the DHT and scraping off, as root with
PUID=0 PGID=0 LODESTAR_ALLOW_ROOT=1 (a sandbox: no other account). It drops
the OS page cache before the cold start when /proc/sys/vm/drop_caches is
writable, and says whether it could.

Steps (README "Procedure"):
  init     start once on a fresh data folder, set the admin password, add
           three name rules through /api/v1/filters/apply (flag, quarantine,
           drop; words in about 1 %, 0.5 % and 0.5 % of names), stop
  block    config.toml with a blocklist file holding 1 % of the source's
           hashes (the floor: counted as filtered, no filter_hits row, as
           at ingest)
  backup0  `lodestar backup` before the stage (the ladder's rule)
  import   `lodestar import start` with the server stopped (write time)
  drain    start the server; time from its launch until every stored
           torrent is searchable and until the job reads `complete`
           (stats polled every 100 ms, `import status` once a second)
  idle     RSS after 15 s idle; have-filter; review queue; sizes per
           component (torrent and file index separately)
  cold     stop, drop caches, start: time to /healthz, first (file) search
  load     mixed queries from 1 client thread, then from 4 (which, with
           the server, saturate the sandbox's 4 cores); p50/p95 per
           class, measured at the client (HTTP included); peak RSS
  insights the UI's Insights round (overview + Fetch page) timed, and the
           load repeated with that round every second
  backup1  backup of the imported store (size, time); restore into a new
           folder (copy + reindex + start until searchable)
  remove   a second (Magnetico) job overlapping 500 of the first job's
           hashes, then `lodestar import remove <job 1>`; checks by
           membership that job 1's own records are gone from store and
           search, and every record job 2 holds remains, with its
           membership and quality rows
"""
import argparse
import concurrent.futures as cf
import hashlib
import json
import os
import random
import re
import shutil
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

PORT = 18090
BASE = "http://127.0.0.1:%d" % PORT
PASSWORD = "smoke-test-password-1"
RUNNING = []
PARTIAL = {}


def now():
    return time.monotonic()


def du(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            try:
                total += os.lstat(os.path.join(root, f)).st_size
            except FileNotFoundError:
                pass
    return total


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return None
    k = max(0, min(len(xs) - 1, int(round(p / 100.0 * len(xs) + 0.5)) - 1))
    return xs[k]


class Lodestar:
    def __init__(self, binary, data, logdir):
        self.bin = os.path.abspath(binary)
        self.data = data
        self.logdir = logdir
        self.env = dict(os.environ, PUID="0", PGID="0", LODESTAR_ALLOW_ROOT="1",
                        LODESTAR_DATA_DIR=data, LODESTAR_HTTP_BIND="127.0.0.1:%d" % PORT,
                        LODESTAR_DHT_ENABLED="false", LODESTAR_SCRAPE_ENABLED="false")
        self.proc = None
        self.cookie = None
        self.n = 0

    def cli(self, *args, check=True):
        t = now()
        r = subprocess.run([self.bin, *args], env=self.env, capture_output=True, text=True)
        dt = now() - t
        self.n += 1
        with open(os.path.join(self.logdir, "cli-%02d-%s.log" % (self.n, args[0])), "w") as f:
            f.write(r.stdout + "\n--- stderr\n" + r.stderr)
        if check and r.returncode != 0:
            raise SystemExit("lodestar %s failed (%d): %s" % (" ".join(args), r.returncode, r.stderr[-2000:]))
        return r, dt

    def start(self):
        """Launches `serve`; returns seconds to /healthz. `self.launched`
        is the launch time."""
        try:
            urllib.request.urlopen(BASE + "/healthz", timeout=1)
            raise SystemExit("something already answers on port %d" % PORT)
        except urllib.error.URLError:
            pass
        self.n += 1
        self.log = os.path.join(self.logdir, "serve-%02d.log" % self.n)
        t = self.launched = now()
        self.proc = subprocess.Popen([self.bin, "serve"], env=self.env,
                                     stdout=open(self.log, "w"), stderr=subprocess.STDOUT)
        RUNNING.append(self.proc)
        while True:
            if self.proc.poll() is not None:
                raise SystemExit("server exited: see " + self.log)
            try:
                urllib.request.urlopen(BASE + "/healthz", timeout=1).read()
                break
            except Exception:
                time.sleep(0.02)
            if now() - t > 120:
                raise SystemExit("server didn't answer /healthz in 120 s")
        return now() - t

    def stop(self):
        self.proc.terminate()
        self.proc.wait(timeout=120)
        self.proc = None

    def rss(self):
        """(child RSS, supervisor + child RSS, child anon RSS, child peak
        RSS): `serve` is a supervisor that runs the server as
        `serve --child`."""
        def status(pid):
            with open("/proc/%d/status" % pid) as f:
                s = f.read()
            return lambda k: int(re.search(k + r":\s+(\d+) kB", s).group(1)) * 1024
        sup = status(self.proc.pid)
        kids = []
        for d in os.listdir("/proc"):
            if d.isdigit():
                try:
                    with open("/proc/%s/stat" % d) as f:
                        ppid = int(f.read().rsplit(")", 1)[1].split()[1])
                    if ppid == self.proc.pid:
                        kids.append(int(d))
                except (OSError, ValueError, IndexError):
                    pass
        if len(kids) != 1:
            raise SystemExit("expected one server child, found %r" % kids)
        ch = status(kids[0])
        return (ch("VmRSS"), ch("VmRSS") + sup("VmRSS"), ch("RssAnon"), ch("VmHWM"))

    def req(self, path, body=None, timeout=60):
        h = {"Content-Type": "application/json", "Origin": BASE}
        if self.cookie:
            h["Cookie"] = self.cookie
        data = json.dumps(body).encode() if body is not None else None
        return urllib.request.urlopen(urllib.request.Request(BASE + path, data=data, headers=h),
                                      timeout=timeout)

    def metrics(self, prefix):
        """`/metrics` samples whose name contains `prefix`."""
        body = self.req("/metrics").read().decode()
        return {l.split()[0]: float(l.split()[1]) for l in body.splitlines()
                if l and not l.startswith("#") and prefix in l.split()[0]}

    def get(self, path):
        return json.load(self.req(path))

    def login(self):
        """Sets up or logs in, accepting the Legal Notice the way the UI
        does: setup sends the version `GET /api/v1/legal/notice` shows
        (without it, `409 notice_required`), and a login that
        `GET /api/v1/auth` says still owes the current notice accepts it
        through `/api/v1/legal/accept`."""
        version = self.get("/api/v1/legal/notice")["version"]
        with open(self.log) as f:
            m = re.search(r"setup token ([0-9A-F-]{19})", f.read())
        if m:
            r = self.req("/api/v1/setup", {"token": m.group(1), "password": PASSWORD,
                                           "notice_version": version})
            self.cookie = r.headers["Set-Cookie"].split(";")[0]
            return
        r = self.req("/api/v1/login", {"password": PASSWORD})
        self.cookie = r.headers["Set-Cookie"].split(";")[0]
        if self.get("/api/v1/auth")["notice_required"]:
            self.req("/api/v1/legal/accept", {"version": version}).read()

    def search(self, q, limit=50):
        t = now()
        r = self.req("/api/v1/search?" + urllib.parse.urlencode({"q": q, "limit": limit}))
        body = r.read()
        return now() - t, json.loads(body)

    def drain(self, launched, expect_job=None, deadline=1800):
        """Polls `/api/v1/stats` every 100 ms until searchable == stored and
        the index lag is 0, and (with expect_job) `lodestar import status`
        once a second until the job reads complete. Returns seconds from
        `launched` (the server's launch) to each, and the last stats."""
        searchable_at = complete_at = None
        last = None
        next_status = now()
        while now() - launched < deadline:
            s = self.get("/api/v1/stats")
            last = s
            if searchable_at is None and s["stored_torrents"] == s["searchable_torrents"] \
                    and s["index_lag_s"] == 0:
                searchable_at = now() - launched
            if expect_job is not None and complete_at is None and \
                    (searchable_at is not None or now() >= next_status):
                next_status = now() + 1.0
                r, _ = self.cli("import", "status", str(expect_job), "--json")
                st = json.loads(r.stdout)
                if (st[0] if isinstance(st, list) else st)["phase"].startswith("complete"):
                    complete_at = now() - launched
            if searchable_at is not None and (expect_job is None or complete_at is not None):
                break
            time.sleep(0.1)
        return searchable_at, complete_at, last


def store_sizes(db):
    """Bytes by component from dbstat, read-only. Provenance = the import_*
    tables, torrent_claims*, unverified_items and their indexes."""
    c = sqlite3.connect("file:%s?mode=ro" % db, uri=True)
    kinds = dict(c.execute("SELECT name, type FROM sqlite_master"))
    owner = dict(c.execute("SELECT name, tbl_name FROM sqlite_master"))
    out = {"tables": 0, "indexes": 0, "provenance": 0, "free": 0}
    per = {}
    for name, size in c.execute("SELECT name, sum(pgsize) FROM dbstat GROUP BY name"):
        tbl = owner.get(name, name)
        if tbl.startswith("import_") or tbl.startswith("torrent_claims") or tbl == "unverified_items":
            k = "provenance"
        elif kinds.get(name) == "index" or name.startswith("sqlite_autoindex"):
            k = "indexes"
        else:
            k = "tables"
        out[k] += size
        per[name] = size
    page = c.execute("PRAGMA page_size").fetchone()[0]
    out["free"] = c.execute("PRAGMA freelist_count").fetchone()[0] * page
    counts = {t: c.execute("SELECT count(*) FROM %s" % t).fetchone()[0]
              for t in ("torrents", "filter_hits", "quarantine", "tombstones",
                        "import_membership", "import_candidates", "unverified_items")}
    c.close()
    return out, per, counts


def data_sizes(data):
    db = os.path.join(data, "lodestar.db")
    files = {k: os.path.getsize(db + k) if os.path.exists(db + k) else 0 for k in ("", "-wal", "-shm")}
    index = os.path.join(data, "index")
    file_index = sum(du(os.path.join(r, d)) for r, ds, _ in os.walk(index) for d in ds if d == "files")
    return {"db": files[""], "wal": files["-wal"], "index": du(index), "file_index": file_index,
            "hot": du(os.path.join(data, "hot")), "total": du(data) - du(os.path.join(data, "backups"))}


def source_hashes(path):
    c = sqlite3.connect("file:%s?mode=ro" % path, uri=True)
    hs = [bytes(r[0]).hex() for r in c.execute("SELECT info_hash FROM torrents ORDER BY id")]
    names = [r[0] for r in c.execute("SELECT name FROM torrents ORDER BY id")]
    c.close()
    return hs, names


def queries(names, rng):
    """Query mix by class, from tokens of the source's own names."""
    toks = {}
    for n in names:
        for t in re.split(r"[^\w]+", n.lower()):
            if len(t) >= 3 and not t.isdigit():
                toks[t] = toks.get(t, 0) + 1
    by = sorted(toks.items(), key=lambda kv: -kv[1])
    common = [t for t, _ in by[:40]]
    rare = [t for t, c in by if c <= 3][:2000]
    mid = [t for t, c in by if 4 <= c <= 100][:2000]
    out = []
    for _ in range(300):
        out.append(("keyword", rng.choice(mid)))
        out.append(("keyword", "%s %s" % (rng.choice(common), rng.choice(mid))))
        out.append(("rare", rng.choice(rare)))
        out.append(("common", rng.choice(common)))
        out.append(("field", rng.choice(["size:>1GB", "files:>=10", "ext:mkv", "size:700MB..1.5GB",
                                          "largest:>2GB", "discovered:2024-01-01..2024-12-31"])))
        out.append(("file", "file:(%s)" % rng.choice(mid)))
        out.append(("file", "file:(ext:flac size:>50MB) %s" % rng.choice(common)))
    rng.shuffle(out)
    return out


def load(ls, qs, secs, threads=4, insights_every=None):
    lat = {}
    errors = 0
    stop = now() + secs
    lock = threading.Lock()
    peak = [0]
    it = iter(qs * 50)
    rounds = []

    def worker():
        nonlocal errors
        while now() < stop:
            with lock:
                cls, q = next(it)
            try:
                dt, _ = ls.search(q)
            except urllib.error.HTTPError as e:
                with lock:
                    errors += 1
                    lat.setdefault("errors_" + str(e.code), []).append(q)
                continue
            with lock:
                lat.setdefault(cls, []).append(dt)

    def sampler():
        while now() < stop:
            peak[0] = max(peak[0], ls.rss()[1])
            if insights_every:
                t = now()
                ls.get("/api/v1/insights/overview")
                ls.get("/api/v1/insights/fetch")
                rounds.append(now() - t)
                time.sleep(max(0, insights_every - (now() - t)))
            else:
                time.sleep(0.2)

    with cf.ThreadPoolExecutor(threads + 1) as ex:
        fs = [ex.submit(worker) for _ in range(threads)] + [ex.submit(sampler)]
        for f in fs:
            f.result()
    res = {}
    alll = []
    for cls, xs in lat.items():
        if cls.startswith("errors_"):
            res[cls] = len(xs)
            continue
        alll += xs
        res[cls] = {"n": len(xs), "p50_ms": round(pct(xs, 50) * 1000, 1),
                    "p95_ms": round(pct(xs, 95) * 1000, 1), "max_ms": round(max(xs) * 1000, 1)}
    res["all"] = {"n": len(alll), "qps": round(len(alll) / secs, 1),
                  "p95_ms": round(pct(alll, 95) * 1000, 1)}
    res["peak_rss"] = peak[0]
    if rounds:
        res["insights_round_ms_mean"] = round(sum(rounds) / len(rounds) * 1000, 1)
    return res


def drop_caches():
    try:
        os.sync()
        with open("/proc/sys/vm/drop_caches", "w") as f:
            f.write("3\n")
        return True
    except OSError:
        return False


def rule_tokens(names):
    """Three distinct whole-name tokens whose share of names is nearest to
    1 %, 0.5 % and 0.5 %: the flag, quarantine and drop rules' words."""
    df = {}
    for n in names:
        for t in set(re.split(r"[^\w]+", n.lower())):
            if len(t) >= 4 and t.isalpha() and t.isascii():
                df[t] = df.get(t, 0) + 1
    out = []
    for share in (0.01, 0.005, 0.005):
        t = min((t for t in df if t not in out), key=lambda t: (abs(df[t] - share * len(names)), t))
        out.append(t)
    return [(t, df[t]) for t in out]


def membership(db, job):
    c = sqlite3.connect("file:%s?mode=ro" % db, uri=True)
    rows = {bytes(r[0]).hex() for r in c.execute(
        "SELECT t.info_hash FROM import_membership m JOIN torrents t ON t.id = m.torrent_id"
        " WHERE m.job = ?", (job,))}
    c.close()
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--source", required=True,
                    help="the Magnetico file; with --kind bitmagnet, the file the database was filled from")
    ap.add_argument("--overlap", required=True, help="a Magnetico file sharing 500 hashes")
    ap.add_argument("--work", required=True)
    ap.add_argument("--load-secs", type=int, default=20)
    ap.add_argument("--kind", choices=["magnetico", "bitmagnet"], default="magnetico")
    ap.add_argument("--pg-host", help="bitmagnet: PostgreSQL's socket folder")
    ap.add_argument("--pg-password-file", help="bitmagnet: the reader's password file")
    a = ap.parse_args()
    work = os.path.abspath(a.work)
    os.makedirs(work, exist_ok=True)
    if os.listdir(work):
        raise SystemExit("WORK isn't empty")
    data, logs = os.path.join(work, "data"), os.path.join(work, "logs")
    os.makedirs(logs)
    db = os.path.join(data, "lodestar.db")
    if a.kind == "magnetico":
        src = ["--source", "magnetico", "--path", a.source]
    else:
        src = ["--source", "bitmagnet", "--host", a.pg_host, "--password-file", a.pg_password_file,
               "--allow-plaintext-loopback"]
    R = PARTIAL
    R |= {"kind": a.kind, "source": os.path.basename(a.source), "source_bytes": os.path.getsize(a.source)}
    ls = Lodestar(a.bin, data, logs)
    hashes, names = source_hashes(a.source)
    n = len(hashes)
    rng = random.Random(20261102)

    # init, and three name rules: flag ~1 %, quarantine ~0.5 %, drop ~0.5 %
    R["cold_start_empty_s"] = round(ls.start(), 3)
    ls.login()
    rules = []
    for (tok, df), action in zip(rule_tokens(names), ("flag", "quarantine", "drop")):
        ls.req("/api/v1/filters/apply",
               {"change": {"op": "add_rule", "query": tok, "action": action}}).read()
        rules.append({"query": tok, "action": action, "names_with_token": df})
    R["rules"] = rules
    time.sleep(5)
    R["rss_empty_idle"] = ls.rss()
    ls.stop()
    empty_store, _, empty_counts = store_sizes(db)
    R["empty"] = dict(data_sizes(data), store=empty_store, counts=empty_counts)

    # block: 1 % of the source's hashes in a config blocklist file (the floor)
    blocked = hashes[::100]
    with open(os.path.join(data, "smoke-blocklist.txt"), "w") as f:
        f.write("\n".join(blocked) + "\n")
    with open(os.path.join(data, "config.toml"), "w") as f:
        f.write('[blocklist]\nfiles = ["%s"]\n' % os.path.join(data, "smoke-blocklist.txt"))

    # backup0
    _, dt = ls.cli("backup", "--to", os.path.join(work, "backup0"))
    R["backup_pre"] = {"s": round(dt, 3), "bytes": du(os.path.join(work, "backup0"))}

    # import
    _, dt = ls.cli("import", "inspect", *src)
    R["inspect_s"] = round(dt, 3)
    r, dt = ls.cli("import", "start", *src, "--json")
    out = json.loads(r.stdout)
    R["import"] = {"write_s": round(dt, 3), "counters": out["counters"], "findings": out["findings"],
                   "ended": out["ended"]}
    job1 = out["job"]
    R["after_import_offline"] = data_sizes(data)

    # drain: from the server's launch to the job reading complete
    st = ls.start()
    ls.login()
    searchable, complete, stats = ls.drain(ls.launched, expect_job=job1)
    R["drain"] = {"server_start_s": round(st, 3), "searchable_s": round(searchable, 3),
                  "job_complete_s": round(complete, 3)}
    w = R["import"]["write_s"]
    R["import"]["end_to_end_s"] = round(w + complete, 3)
    R["import"]["end_to_end_per_s"] = round(n / (w + complete), 1)
    R["import"]["write_per_s"] = round(n / w, 1)

    # idle
    time.sleep(15)
    R["rss_idle"] = ls.rss()
    R["have_filter"] = ls.metrics("have_")
    q = ls.get("/api/v1/filters/queue")
    R["review_queue"] = {k: (len(v) if isinstance(v, list) else v) for k, v in q.items()}
    rounds = []
    for _ in range(10):
        t = now()
        ls.get("/api/v1/insights/overview")
        ls.get("/api/v1/insights/fetch")
        rounds.append(now() - t)
    R["insights_round_ms"] = {"mean": round(sum(rounds) / 10 * 1000, 1),
                              "max": round(max(rounds) * 1000, 1),
                              "share_of_30s_pct": round(sum(rounds) / 10 / 30 * 100, 3)}
    ls.stop()
    store, per, counts = store_sizes(db)
    c = sqlite3.connect("file:%s?mode=ro" % db, uri=True)
    counts["flagged"] = c.execute(
        "SELECT count(*) FROM torrents WHERE flags & ?", (1 << 13,)).fetchone()[0]  # flags::FLAGGED
    counts["filter_hits_by_action"] = dict(c.execute(
        "SELECT action, count(*) FROM filter_hits GROUP BY action"))
    c.close()
    sizes = data_sizes(data)
    R["sizes"] = dict(sizes, store=store, counts=counts,
                      top_objects=dict(sorted(per.items(), key=lambda kv: -kv[1])[:12]))
    stored = counts["torrents"]
    e = R["empty"]
    R["bytes_per_record"] = {
        "basis": stored,
        "sqlite_tables": round((store["tables"] - empty_store["tables"]) / stored, 1),
        "sqlite_indexes": round((store["indexes"] - empty_store["indexes"]) / stored, 1),
        "provenance": round((store["provenance"] - empty_store["provenance"]) / stored, 1),
        "sqlite_file": round((sizes["db"] - e["db"]) / stored, 1),
        "search_index": round((sizes["index"] - e["index"]) / stored, 1),
        "file_index": round((sizes["file_index"] - e["file_index"]) / stored, 1),
        "torrent_index": round((sizes["index"] - sizes["file_index"]
                                - e["index"] + e["file_index"]) / stored, 1),
        "hot": round((sizes["hot"] - e["hot"]) / stored, 1),
        "all_in": round((sizes["total"] - e["total"]) / stored, 1),
    }

    # cold
    R["page_cache_dropped"] = drop_caches()
    st = ls.start()
    ls.login()
    dt, body = ls.search("file:(ext:flac size:>50MB)")
    R["cold"] = {"start_s": round(st, 3), "first_file_search_ms": round(dt * 1000, 1),
                 "first_file_search_hits": len(body.get("hits", []))}
    dt, _ = ls.search("file:(ext:flac size:>50MB)")
    R["cold"]["second_same_ms"] = round(dt * 1000, 1)
    R["rss_after_cold_start"] = ls.rss()

    # load, then load with Insights every second
    qs = queries(names, rng)
    for cls, q in qs[:50]:
        ls.search(q)
    R["load_1client"] = load(ls, qs, a.load_secs, threads=1)
    R["load"] = load(ls, qs, a.load_secs)
    R["load_insights_1s"] = load(ls, qs, a.load_secs, insights_every=1.0)
    R["rss_after_load"] = ls.rss()
    ls.stop()

    # backup1 + restore
    _, dt = ls.cli("backup", "--to", os.path.join(work, "backup1"))
    R["backup_post"] = {"s": round(dt, 3), "bytes": du(os.path.join(work, "backup1"))}
    rdata = os.path.join(work, "restore")
    os.makedirs(os.path.join(work, "logs-restore"))
    rl = Lodestar(a.bin, rdata, os.path.join(work, "logs-restore"))
    (b1,) = os.listdir(os.path.join(work, "backup1"))  # backup --to makes one folder
    b1 = os.path.join(work, "backup1", b1)
    t = now()
    os.makedirs(rdata)
    for fn in os.listdir(b1):
        if fn != "README.txt":
            shutil.copy2(os.path.join(b1, fn), rdata)
    copy_s = now() - t
    rl.cli("reindex")
    st = rl.start()
    rl.login()
    searchable, _, stats = rl.drain(rl.launched)
    R["restore"] = {"copy_s": round(copy_s, 3), "start_s": round(st, 3),
                    "total_to_searchable_s": round(now() - t, 3),
                    "searchable": stats["searchable_torrents"], "stored": stats["stored_torrents"]}
    rl.stop()
    shutil.rmtree(rdata)
    shutil.rmtree(os.path.join(work, "backup0"))

    # remove: job 2 (Magnetico, overlapping) then job 1's records
    ohashes, _ = source_hashes(a.overlap)
    r, dt = ls.cli("import", "start", "--source", "magnetico", "--path", a.overlap, "--json")
    out = json.loads(r.stdout)
    job2 = out["job"]
    R["overlap_import"] = {"s": round(dt, 3), "counters": out["counters"]}
    ls.start()
    ls.login()
    ls.drain(ls.launched, expect_job=job2)
    before = ls.get("/api/v1/stats")["stored_torrents"]
    ls.stop()
    # What job 2 holds (new or merged) must survive; what only job 1 holds
    # must go. Held-back records (floor, rules) have no membership.
    m1, m2 = membership(db, job1), membership(db, job2)
    only1, kept, shared = sorted(m1 - m2), sorted(m2), sorted(m1 & m2)
    r, dt = ls.cli("import", "remove", str(job1), "--yes", "--json")
    R["remove"] = {"s": round(dt, 3), "report": json.loads(r.stdout), "stored_before": before,
                   "job1_members": len(m1), "job2_members": len(m2), "shared_members": len(shared)}
    ls.start()
    ls.login()
    searchable, _, stats = ls.drain(ls.launched)
    ck = {"stored_after": stats["stored_torrents"], "searchable_after": stats["searchable_torrents"],
          "expected_after": len(kept), "reindex_s": round(searchable, 3)}
    c = sqlite3.connect("file:%s?mode=ro" % db, uri=True)
    present = {bytes(r[0]).hex() for r in c.execute("SELECT info_hash FROM torrents")}
    ck["job1_membership_rows_left"] = c.execute(
        "SELECT count(*) FROM import_membership WHERE job = ?", (job1,)).fetchone()[0]
    ck["job2_membership_rows"] = c.execute(
        "SELECT count(*) FROM import_membership WHERE job = ?", (job2,)).fetchone()[0]
    ck["import_quality_rows"] = c.execute("SELECT count(*) FROM import_quality").fetchone()[0]
    c.close()
    ck["job1_only_left_in_store"] = sum(1 for h in only1 if h in present)
    ck["job2_missing_from_store"] = sum(1 for h in kept if h not in present)
    sample1 = random.Random(7).sample(only1, 50)
    sample2 = random.Random(8).sample(shared, 25) + random.Random(9).sample(
        sorted(set(kept) - set(shared)), 25)
    ck["job1_sample_found_by_search"] = sum(1 for h in sample1 if ls.search("hash:" + h)[1]["hits"])
    ck["job2_sample_found_by_search"] = sum(1 for h in sample2 if ls.search("hash:" + h)[1]["hits"])
    R["remove"]["check"] = ck
    ls.stop()
    print(json.dumps(R, indent=1, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except BaseException:
        print(json.dumps(PARTIAL, indent=1, sort_keys=True, default=str))
        raise
    finally:
        for p in RUNNING:
            if p.poll() is None:
                p.kill()
