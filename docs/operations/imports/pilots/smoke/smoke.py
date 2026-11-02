#!/usr/bin/env python3
"""G-import-smoke stage driver: runs the offline import ladder for one stage
and measures build-plan §0.6's capacity list. Prints one JSON object of
aggregates (the only output the report uses; raw logs stay in WORK).

Usage:
  smoke.py --bin target/release/lodestar --source m10k.sqlite3 \
           --overlap o10k.sqlite3 --work /path/to/empty/dir [--load-secs 20]

WORK must not exist (or be empty). The driver runs the server on
127.0.0.1:18090 with the DHT and scraping off, as root with
PUID=0 PGID=0 LODESTAR_ALLOW_ROOT=1 (a sandbox: no other account). It drops
the OS page cache before the cold start when /proc/sys/vm/drop_caches is
writable, and says whether it could.

Steps (README "Procedure"):
  init     start once on a fresh data folder, set the admin password, stop
  block    config.toml with a blocklist file holding 1 % of the source's
           hashes, so the import's filter path (filter_hits) is exercised
  backup0  `lodestar backup` before the stage (the ladder's rule)
  import   `lodestar import start` with the server stopped (write time)
  drain    start the server, time until every stored torrent is searchable
           and until the job reads `complete` (index drain)
  idle     RSS after 15 s idle; sizes per component
  cold     stop, drop caches, start: time to /healthz, first (file) search
  load     mixed queries from 1 client thread, then from 4 (which, with
           the server, saturate the sandbox's 4 cores); p50/p95 per
           class, measured at the client (HTTP included); peak RSS
  insights the UI's Insights round (overview + Fetch page) timed, and the
           load repeated with that round every second
  backup1  backup of the imported store (size, time); restore into a new
           folder (copy + reindex + start until searchable)
  remove   a second job overlapping 500 of the first job's hashes, then
           `lodestar import remove <job 1>`; checks the first job's
           records are gone from store and search, and the second job's
           records (including the 500 shared ones) remain
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
        try:
            urllib.request.urlopen(BASE + "/healthz", timeout=1)
            raise SystemExit("something already answers on port %d" % PORT)
        except urllib.error.URLError:
            pass
        self.n += 1
        self.log = os.path.join(self.logdir, "serve-%02d.log" % self.n)
        t = now()
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
        with open(self.log) as f:
            m = re.search(r"setup token ([0-9A-F-]{19})", f.read())
        if m:
            r = self.req("/api/v1/setup", {"token": m.group(1), "password": PASSWORD})
        else:
            r = self.req("/api/v1/login", {"password": PASSWORD})
        self.cookie = r.headers["Set-Cookie"].split(";")[0]

    def search(self, q, limit=50):
        t = now()
        r = self.req("/api/v1/search?" + urllib.parse.urlencode({"q": q, "limit": limit}))
        body = r.read()
        return now() - t, json.loads(body)

    def drain(self, expect_job=None, deadline=1800):
        """Polls until searchable == stored and the index lag is 0, and
        (with expect_job) until the job reads complete. Returns seconds
        from the call to each."""
        t = now()
        searchable_at = complete_at = None
        last = None
        while now() - t < deadline:
            s = self.get("/api/v1/stats")
            last = s
            if searchable_at is None and s["stored_torrents"] == s["searchable_torrents"] \
                    and s["index_lag_s"] == 0:
                searchable_at = now() - t
            if expect_job is not None and complete_at is None:
                r, _ = self.cli("import", "status", str(expect_job), "--json")
                st = json.loads(r.stdout)
                if (st[0] if isinstance(st, list) else st)["phase"].startswith("complete"):
                    complete_at = now() - t
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
    return {"db": files[""], "wal": files["-wal"], "index": du(os.path.join(data, "index")),
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--source", required=True)
    ap.add_argument("--overlap", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--load-secs", type=int, default=20)
    a = ap.parse_args()
    work = os.path.abspath(a.work)
    os.makedirs(work, exist_ok=True)
    if os.listdir(work):
        raise SystemExit("WORK isn't empty")
    data, logs = os.path.join(work, "data"), os.path.join(work, "logs")
    os.makedirs(logs)
    R = PARTIAL
    R |= {"source": os.path.basename(a.source), "source_bytes": os.path.getsize(a.source)}
    ls = Lodestar(a.bin, data, logs)
    hashes, names = source_hashes(a.source)
    n = len(hashes)
    rng = random.Random(20261102)

    # init
    R["cold_start_empty_s"] = round(ls.start(), 3)
    ls.login()
    time.sleep(5)
    R["rss_empty_idle"] = ls.rss()
    ls.stop()
    empty_store, _, _ = store_sizes(os.path.join(data, "lodestar.db"))
    R["empty"] = dict(data_sizes(data), store=empty_store)

    # block: 1 % of the source's hashes in a config blocklist file
    blocked = hashes[::100]
    with open(os.path.join(data, "smoke-blocklist.txt"), "w") as f:
        f.write("\n".join(blocked) + "\n")
    with open(os.path.join(data, "config.toml"), "w") as f:
        f.write('[blocklist]\nfiles = ["%s"]\n' % os.path.join(data, "smoke-blocklist.txt"))

    # backup0
    _, dt = ls.cli("backup", "--to", os.path.join(work, "backup0"))
    R["backup_pre"] = {"s": round(dt, 3), "bytes": du(os.path.join(work, "backup0"))}

    # import
    _, dt = ls.cli("import", "inspect", "--source", "magnetico", "--path", a.source)
    R["inspect_s"] = round(dt, 3)
    r, dt = ls.cli("import", "start", "--source", "magnetico", "--path", a.source, "--json")
    out = json.loads(r.stdout)
    R["import"] = {"write_s": round(dt, 3), "counters": out["counters"], "findings": out["findings"],
                   "ended": out["ended"]}
    job1 = out["job"]
    R["after_import_offline"] = data_sizes(data)

    # drain
    t = now()
    st = ls.start()
    ls.login()
    searchable, complete, stats = ls.drain(expect_job=job1)
    R["drain"] = {"server_start_s": round(st, 3), "searchable_s": round(st + searchable, 3),
                  "job_complete_s": round(st + complete, 3), "stats": stats}
    w = R["import"]["write_s"]
    R["import"]["end_to_end_s"] = round(w + st + searchable, 3)
    R["import"]["end_to_end_per_s"] = round(n / (w + st + searchable), 1)
    R["import"]["write_per_s"] = round(n / w, 1)

    # idle
    time.sleep(15)
    R["rss_idle"] = ls.rss()
    R["filters"] = ls.get("/api/v1/filters")
    R["have_filter"] = ls.metrics("have_")
    try:
        q = ls.get("/api/v1/filters/queue")
        R["review_queue"] = {k: (len(v) if isinstance(v, list) else v) for k, v in q.items()}
    except urllib.error.HTTPError as e:
        R["review_queue"] = "HTTP %d" % e.code
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
    store, per, counts = store_sizes(os.path.join(data, "lodestar.db"))
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
    rl = Lodestar(a.bin, rdata, logs)
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
    searchable, _, stats = rl.drain()
    R["restore"] = {"copy_s": round(copy_s, 3), "start_s": round(st, 3),
                    "total_to_searchable_s": round(now() - t, 3),
                    "searchable": stats["searchable_torrents"], "stored": stats["stored_torrents"]}
    rl.stop()
    shutil.rmtree(rdata)
    shutil.rmtree(os.path.join(work, "backup0"))

    # remove
    ohashes, _ = source_hashes(a.overlap)
    r, dt = ls.cli("import", "start", "--source", "magnetico", "--path", a.overlap, "--json")
    out = json.loads(r.stdout)
    job2 = out["job"]
    R["overlap_import"] = {"s": round(dt, 3), "counters": out["counters"]}
    st = ls.start()
    ls.login()
    ls.drain(expect_job=job2)
    before = ls.get("/api/v1/stats")["stored_torrents"]
    ls.stop()
    r, dt = ls.cli("import", "remove", str(job1), "--yes", "--json")
    R["remove"] = {"s": round(dt, 3), "report": json.loads(r.stdout), "stored_before": before}
    st = ls.start()
    ls.login()
    searchable, _, stats = ls.drain()
    # The blocklist holds 1 % of job 1's hashes, some of them shared with
    # job 2: neither job stored those.
    bl = set(blocked)
    shared = (set(hashes) & set(ohashes)) - bl
    only1 = [h for h in hashes if h not in shared and h not in bl]
    kept = [h for h in ohashes if h not in bl]
    kept_expected = len(kept)
    ck = {"stored_after": stats["stored_torrents"], "searchable_after": stats["searchable_torrents"],
          "expected_after": kept_expected, "reindex_s": round(st + searchable, 3)}
    c = sqlite3.connect("file:%s?mode=ro" % os.path.join(data, "lodestar.db"), uri=True)
    present = {bytes(r[0]).hex() for r in c.execute("SELECT info_hash FROM torrents")}
    c.close()
    ck["job1_only_left_in_store"] = sum(1 for h in only1 if h in present)
    ck["job2_missing_from_store"] = sum(1 for h in kept if h not in present)
    sample1 = random.Random(7).sample(only1, 50)
    sample2 = random.Random(8).sample(sorted(shared), 25) + random.Random(9).sample(
        sorted(set(kept) - shared), 25)
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
