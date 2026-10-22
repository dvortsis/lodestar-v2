//! The relevance suite (architecture §8, §14 M8, §17): a fixed,
//! synthetic judgment corpus, about 150 queries judged 0–3, and the
//! measures that keep ranking changes honest.
//!
//! It builds a store and index from `bench/relevance/corpus.txt` the way
//! the server does (insert, swarm scrape, enrichment, indexing), runs
//! every query of `bench/relevance/judgments.txt` through the search
//! path `/search` takes (a searcher lease and its first page, with the
//! default visibility: filter-hidden torrents and Adult left out), and
//! reports NDCG@10, MRR and recall@50 per query and overall, worst
//! queries first.
//!
//! With `--gate` it exits non-zero when overall NDCG@10 falls more than
//! the tolerance below `bench/relevance/baseline.txt`, when a torrent
//! judged `x` (must never be shown) is in a query's results, or when the
//! corpus or judgments changed since the baseline was written.
//! `--write-baseline` records the current figures; the decision record
//! `docs/decisions/2026-10-22-R-relevance.md` says when that's right.
//!
//! The measures:
//! - **NDCG@10:** gain `2^grade − 1`, discount `log2(rank + 1)`, over
//!   the ideal order of every judged torrent. A query with nothing to
//!   find (only `x` judgments) scores 1 when nothing forbidden shows
//!   and 0 when something does.
//! - **MRR:** one over the rank of the first result graded 2 or 3 (or 1,
//!   when nothing is graded higher), within the top 50.
//! - **Recall@50:** the share of torrents graded 1–3 in the top 50.
//!
//! MRR and recall average over the queries that have something to find.
//! Unjudged results count as grade 0.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::query::TagKey;
use lodestar_search::{CancelToken, Lease, Node, SearchIndex, Sort, Start, index_dir, parse};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, SwarmUpdate, WriteBatch};

/// Results read per query: recall's depth, and where forbidden torrents
/// are looked for.
const DEPTH: usize = 50;
/// NDCG's depth.
const NDCG_AT: usize = 10;
/// How far overall NDCG@10 may fall below the baseline.
const TOLERANCE: f64 = 0.01;
/// The corpus's clock: the first torrent is discovered here, the rest an
/// hour apart, and "now" is a day after the last. Nothing reads the real
/// clock, so every run ranks the same.
const EPOCH: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

const USAGE: &str = "\
Usage: relevance [--dir PATH] [--gate] [--tolerance T] [--worst N] [--all] [--write-baseline]

  --dir PATH         the corpus, judgments and baseline (default bench/relevance)
  --gate             exit 1 on a regression past the tolerance, a forbidden
                     result, or inputs changed since the baseline
  --tolerance T      allowed NDCG@10 drop below the baseline (default 0.01)
  --worst N          worst queries listed with their results (default 10)
  --all              list every query
  --write-baseline   record these figures as the baseline";

struct Args {
    dir: PathBuf,
    gate: bool,
    tolerance: f64,
    worst: usize,
    all: bool,
    write_baseline: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("relevance"),
        gate: false,
        tolerance: TOLERANCE,
        worst: 10,
        all: false,
        write_baseline: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--dir" => a.dir = PathBuf::from(value("--dir")?),
            "--gate" => a.gate = true,
            "--tolerance" => {
                a.tolerance = value("--tolerance")?
                    .parse()
                    .map_err(|_| "bad --tolerance")?
            }
            "--worst" => a.worst = value("--worst")?.parse().map_err(|_| "bad --worst")?,
            "--all" => a.all = true,
            "--write-baseline" => a.write_baseline = true,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    Ok(a)
}

// ---- the corpus -----------------------------------------------------------

/// One fixture torrent.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    key: String,
    name: String,
    /// Single-file torrents have one file with an empty path.
    files: Vec<(Vec<String>, u64)>,
    /// From a scrape; `None` was never scraped.
    seeders: Option<u32>,
}

/// Parses `corpus.txt`:
///
/// ```text
/// # a comment
/// key [seeders=N] | size | name          a single-file torrent
/// key [seeders=N] | dir  | name          a multi-file torrent, whose files
///     size | path/to/file.mkv            follow, indented
/// ```
///
/// Sizes take `K`, `M`, `G` and `T` suffixes (powers of 1024).
fn parse_corpus(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut keys = HashSet::new();
    let mut open_dir = false;
    for (n, line) in text.lines().enumerate() {
        let at = |msg: &str| format!("corpus.txt:{}: {msg}", n + 1);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            let last = entries
                .last_mut()
                .filter(|_| open_dir)
                .ok_or_else(|| at("a file line outside a multi-file torrent"))?;
            let (size, path) = trimmed
                .split_once('|')
                .ok_or_else(|| at("expected `size | path`"))?;
            let path: Vec<String> = path.trim().split('/').map(str::to_string).collect();
            if path.iter().any(String::is_empty) {
                return Err(at("empty path segment"));
            }
            last.files
                .push((path, parse_size(size.trim()).map_err(|e| at(&e))?));
            continue;
        }
        if open_dir && entries.last().is_some_and(|e| e.files.is_empty()) {
            return Err(at("the previous multi-file torrent has no files"));
        }
        let mut parts = trimmed.splitn(3, '|').map(str::trim);
        let (Some(head), Some(size), Some(name)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(at("expected `key | size | name`"));
        };
        let mut head = head.split_whitespace();
        let key = head.next().ok_or_else(|| at("missing key"))?.to_string();
        let mut seeders = None;
        for attr in head {
            match attr.split_once('=') {
                Some(("seeders", v)) => {
                    seeders = Some(v.parse().map_err(|_| at("bad seeders"))?);
                }
                _ => return Err(at(&format!("unknown attribute {attr}"))),
            }
        }
        if !keys.insert(key.clone()) {
            return Err(at(&format!("duplicate key {key}")));
        }
        if name.is_empty() {
            return Err(at("empty name"));
        }
        open_dir = size == "dir";
        let files = if open_dir {
            Vec::new()
        } else {
            vec![(Vec::new(), parse_size(size).map_err(|e| at(&e))?)]
        };
        entries.push(Entry {
            key,
            name: name.to_string(),
            files,
            seeders,
        });
    }
    if open_dir && entries.last().is_some_and(|e| e.files.is_empty()) {
        return Err("corpus.txt: the last multi-file torrent has no files".into());
    }
    Ok(entries)
}

fn parse_size(s: &str) -> Result<u64, String> {
    let (num, mult) = match s.char_indices().last() {
        Some((i, 'K')) => (&s[..i], 1u64 << 10),
        Some((i, 'M')) => (&s[..i], 1 << 20),
        Some((i, 'G')) => (&s[..i], 1 << 30),
        Some((i, 'T')) => (&s[..i], 1 << 40),
        _ => (s, 1),
    };
    let v: f64 = num.parse().map_err(|_| format!("bad size {s:?}"))?;
    if !(v.is_finite() && v > 0.0) {
        return Err(format!("bad size {s:?}"));
    }
    Ok((v * mult as f64).round() as u64)
}

/// The entry's info dictionary. Piece hashes are placeholders derived
/// from the key, so every entry has its own info hash even where two
/// share a name and layout (a re-upload).
fn info_dict(e: &Entry) -> Vec<u8> {
    let int = |v: u64| OwnedValue::Int(v as i64);
    let bytes = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let total: u64 = e.files.iter().map(|(_, s)| s).sum();
    let piece_len = piece_length(total);
    let pieces = total.div_ceil(piece_len) as usize;
    let seed = fnv1a(e.key.as_bytes()).to_le_bytes();
    let pieces = OwnedValue::Bytes(
        (0..pieces * 20)
            .map(|i| seed[i % 8] ^ (i / 8) as u8)
            .collect(),
    );
    let mut d: BTreeMap<Vec<u8>, OwnedValue> = BTreeMap::new();
    let mut put = |k: &str, v: OwnedValue| d.insert(k.as_bytes().to_vec(), v);
    match e.files.as_slice() {
        [(path, size)] if path.is_empty() => {
            put("length", int(*size));
        }
        files => {
            let list = files
                .iter()
                .map(|(path, size)| {
                    let mut f = BTreeMap::new();
                    f.insert(b"length".to_vec(), int(*size));
                    f.insert(
                        b"path".to_vec(),
                        OwnedValue::List(path.iter().map(|s| bytes(s)).collect()),
                    );
                    OwnedValue::Dict(f)
                })
                .collect();
            put("files", OwnedValue::List(list));
        }
    }
    put("name", bytes(&e.name));
    put("piece length", int(piece_len));
    put("pieces", pieces);
    to_vec(&OwnedValue::Dict(d))
}

/// The smallest power of two ≥ 16 KiB giving at most ~2,000 pieces.
fn piece_length(total: u64) -> u64 {
    let mut len = 16u64 << 10;
    while total / len > 2000 && len < 16 << 20 {
        len *= 2;
    }
    len
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

// ---- the judgments --------------------------------------------------------

/// What a judgment says about one torrent for one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grade {
    /// 0–3: not relevant, related, a right answer, exactly what's asked.
    Graded(u8),
    /// Must never be in the results (Adult while it's hidden, a negated
    /// word).
    Forbidden,
}

#[derive(Debug, Clone, PartialEq)]
struct Judged {
    query: String,
    why: String,
    grades: HashMap<String, Grade>,
}

/// Parses `judgments.txt`:
///
/// ```text
/// # a comment
/// ? the query, as typed
/// = one line on what the ideal results are, and why
/// 3 key key …      exactly what's asked
/// 2 key …          a right answer
/// 1 key …          related, acceptable further down
/// 0 key …          judged not relevant (the same as unjudged)
/// x key …          must never be shown
/// ```
fn parse_judgments(text: &str, known: &HashSet<&str>) -> Result<Vec<Judged>, String> {
    let mut out: Vec<Judged> = Vec::new();
    let mut seen = HashSet::new();
    for (n, line) in text.lines().enumerate() {
        let at = |msg: &str| format!("judgments.txt:{}: {msg}", n + 1);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (tag, rest) = line.split_at(1);
        let rest = rest.trim();
        if tag == "?" {
            if rest.is_empty() {
                return Err(at("empty query"));
            }
            if !seen.insert(rest.to_string()) {
                return Err(at(&format!("query {rest:?} is judged twice")));
            }
            out.push(Judged {
                query: rest.to_string(),
                why: String::new(),
                grades: HashMap::new(),
            });
            continue;
        }
        let last = out
            .last_mut()
            .ok_or_else(|| at("a judgment before any query"))?;
        let grade = match tag {
            "=" => {
                if !last.why.is_empty() {
                    return Err(at("a second rationale line"));
                }
                last.why = rest.to_string();
                continue;
            }
            "0" | "1" | "2" | "3" => Grade::Graded(tag.as_bytes()[0] - b'0'),
            "x" => Grade::Forbidden,
            _ => return Err(at("expected `?`, `=`, a grade 0–3 or `x`")),
        };
        for key in rest.split_whitespace() {
            if !known.contains(key) {
                return Err(at(&format!("unknown torrent {key}")));
            }
            if last.grades.insert(key.to_string(), grade).is_some() {
                return Err(at(&format!("{key} is judged twice for this query")));
            }
        }
    }
    if let Some(j) = out.iter().find(|j| j.why.is_empty()) {
        return Err(format!("judgments.txt: {:?} has no rationale", j.query));
    }
    Ok(out)
}

// ---- the measures ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct Scores {
    ndcg: f64,
    /// `None`: nothing to find.
    mrr: Option<f64>,
    recall: Option<f64>,
    /// Forbidden torrents in the results.
    forbidden: usize,
}

fn gain(g: u8) -> f64 {
    f64::from((1u32 << g) - 1)
}

fn discount(rank: usize) -> f64 {
    ((rank + 2) as f64).log2()
}

/// Scores ranked results (keys, best first) against a query's grades.
fn score(results: &[&str], grades: &HashMap<String, Grade>) -> Scores {
    let grade_of = |k: &str| match grades.get(k) {
        Some(Grade::Graded(g)) => *g,
        _ => 0,
    };
    let forbidden = results
        .iter()
        .take(DEPTH)
        .filter(|k| grades.get(**k) == Some(&Grade::Forbidden))
        .count();
    let mut ideal: Vec<u8> = grades
        .values()
        .filter_map(|g| match g {
            Grade::Graded(g) if *g > 0 => Some(*g),
            _ => None,
        })
        .collect();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    let dcg = |gs: &mut dyn Iterator<Item = u8>| -> f64 {
        // A fold, not `sum`: an empty float sum is −0.0.
        gs.take(NDCG_AT)
            .enumerate()
            .fold(0.0, |dcg, (i, g)| dcg + gain(g) / discount(i))
    };
    let idcg = dcg(&mut ideal.iter().copied());
    let ndcg = if idcg == 0.0 {
        if forbidden == 0 { 1.0 } else { 0.0 }
    } else {
        dcg(&mut results.iter().map(|k| grade_of(k))) / idcg
    };
    let (mrr, recall) = match ideal.first() {
        None => (None, None),
        Some(&best) => {
            let wanted = best.min(2);
            let mrr = results
                .iter()
                .take(DEPTH)
                .position(|k| grade_of(k) >= wanted)
                .map_or(0.0, |i| 1.0 / (i + 1) as f64);
            let found = results
                .iter()
                .take(DEPTH)
                .filter(|k| grade_of(k) > 0)
                .count();
            (Some(mrr), Some(found as f64 / ideal.len() as f64))
        }
    };
    Scores {
        ndcg,
        mrr,
        recall,
        forbidden,
    }
}

fn mean(v: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = v.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    if n == 0 { 0.0 } else { sum / n as f64 }
}

// ---- the baseline ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct Baseline {
    ndcg: f64,
    mrr: f64,
    recall: f64,
    queries: usize,
    /// FNV-1a of the corpus and judgments, line endings normalised.
    inputs: String,
}

fn parse_baseline(text: &str) -> Result<Baseline, String> {
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("baseline.txt: bad line {line:?}"))?;
        fields.insert(k, v.trim());
    }
    let get = |k: &str| {
        fields
            .get(k)
            .copied()
            .ok_or_else(|| format!("baseline.txt: missing {k}"))
    };
    let num = |k: &str| -> Result<f64, String> {
        get(k)?
            .parse()
            .map_err(|_| format!("baseline.txt: bad {k}"))
    };
    Ok(Baseline {
        ndcg: num("ndcg@10")?,
        mrr: num("mrr")?,
        recall: num("recall@50")?,
        queries: get("queries")?
            .parse()
            .map_err(|_| "baseline.txt: bad queries")?,
        inputs: get("inputs")?.to_string(),
    })
}

fn format_baseline(b: &Baseline) -> String {
    format!(
        "# The relevance gate's baseline (bench/README.md, \"Relevance\").\n\
         # Written by `relevance --write-baseline`; update it only as\n\
         # docs/decisions/2026-10-22-R-relevance.md describes.\n\
         ndcg@10 {:.4}\nmrr {:.4}\nrecall@50 {:.4}\nqueries {}\ninputs {}\n",
        b.ndcg, b.mrr, b.recall, b.queries, b.inputs
    )
}

fn inputs_hash(corpus: &str, judgments: &str) -> String {
    let norm = |s: &str| s.replace("\r\n", "\n");
    let mut all = norm(corpus).into_bytes();
    all.push(0);
    all.extend(norm(judgments).into_bytes());
    format!("{:016x}", fnv1a(&all))
}

// ---- the run --------------------------------------------------------------

/// Stores, enriches and indexes the corpus in `dir`; returns the index
/// and each torrent id's key.
fn build(
    dir: &Path,
    corpus: &[Entry],
) -> Result<(SearchIndex, HashMap<u64, String>), Box<dyn std::error::Error>> {
    let mut store = Store::open(dir.join("relevance.db"), StoreConfig::default())?;
    let raw: Vec<Vec<u8>> = corpus.iter().map(info_dict).collect();
    let infos: Vec<Info> = raw
        .iter()
        .map(|r| Info::parse(r))
        .collect::<Result<_, _>>()?;
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .enumerate()
        .map(|(i, info)| NewTorrent {
            info,
            raw_info: None,
            discovered_at: EPOCH + i as i64 * 3600,
            metadata_at: EPOCH + i as i64 * 3600 + 5,
            extra_flags: 0,
            source: SourceId::new(0).expect("valid"),
        })
        .collect();
    let mut keys = HashMap::new();
    let mut ids = Vec::with_capacity(corpus.len());
    for (e, inserted) in corpus.iter().zip(store.insert(&batch)?) {
        let lodestar_store::Inserted::New(id) = inserted else {
            return Err(format!("{} has the same info hash as another entry", e.key).into());
        };
        keys.insert(id as u64, e.key.clone());
        ids.push(id);
    }
    // Swarm health as a scrape records it (§8's priors read it once
    // they land).
    let at = now(corpus.len());
    let swarm: Vec<SwarmUpdate> = corpus
        .iter()
        .zip(&ids)
        .filter_map(|(e, id)| {
            e.seeders.map(|seeders| SwarmUpdate {
                torrent_id: *id,
                at,
                seeders,
                leechers: seeders / 4,
                completed: seeders * 3,
                trackers_reporting: u16::from(seeders > 0),
                trackers_checked: 1,
                dht: None,
                per_tracker: Vec::new(),
                next_scrape_at: at + 86_400,
                flags: 0,
                interest: false,
            })
        })
        .collect();
    store.write(&WriteBatch {
        swarm: &swarm,
        ..WriteBatch::default()
    })?;
    // Enrichment (§6): titles, release fields, junk and Adult.
    let mut after = 0;
    while let Some((last, _)) = lodestar_pipeline::enrich::enrich_stored(&mut store, after, 1000)? {
        after = last;
    }
    // One writer thread and one commit: one segment, so ties break the
    // same way every run.
    let index = SearchIndex::open(&index_dir(&dir.join("index")))?;
    let mut indexer = index.writer(64 << 20)?;
    while indexer.catch_up(&mut store, 10_000)? > 0 {}
    indexer.commit()?;
    indexer.finish()?;
    Ok((index, keys))
}

fn now(torrents: usize) -> i64 {
    EPOCH + torrents as i64 * 3600 + 86_400
}

/// `node` with `/search`'s default visibility (`lodestar_api`'s
/// `base_query`): torrents a filter rule hid and Adult left out.
fn shown(node: Node) -> Node {
    let not_adult = Node::Not {
        of: Box::new(Node::Tag {
            key: TagKey::Type,
            any: vec!["adult".into()],
        }),
    };
    match node.without_hidden() {
        Node::And { mut of } => {
            of.push(not_adult);
            Node::And { of }
        }
        other => Node::And {
            of: vec![other, not_adult],
        },
    }
}

struct Outcome<'j> {
    judged: &'j Judged,
    results: Vec<String>,
    scores: Scores,
}

fn run(a: &Args) -> Result<bool, Box<dyn std::error::Error>> {
    let read = |f: &str| {
        std::fs::read_to_string(a.dir.join(f))
            .map_err(|e| format!("{}: {e}", a.dir.join(f).display()))
    };
    let corpus_text = read("corpus.txt")?;
    let judgments_text = read("judgments.txt")?;
    let corpus = parse_corpus(&corpus_text)?;
    let known: HashSet<&str> = corpus.iter().map(|e| e.key.as_str()).collect();
    let judged = parse_judgments(&judgments_text, &known)?;
    let inputs = inputs_hash(&corpus_text, &judgments_text);

    let tmp = tempfile::tempdir()?;
    let (index, keys) = build(tmp.path(), &corpus)?;
    let index = Arc::new(index);
    let cancel = CancelToken::new();
    let now = now(corpus.len());

    let mut outcomes = Vec::with_capacity(judged.len());
    for j in &judged {
        let node = parse(&j.query).map_err(|e| format!("{:?}: {e}", j.query))?;
        let lease = Lease::new(index.clone(), &shown(node), now, Sort::Relevance, &cancel)?;
        let page = lease.page(Start::Offset(0), DEPTH, &cancel)?;
        let results: Vec<String> = page
            .hits
            .iter()
            .map(|h| {
                keys.get(&h.id)
                    .cloned()
                    .unwrap_or_else(|| format!("#{}", h.id))
            })
            .collect();
        let refs: Vec<&str> = results.iter().map(String::as_str).collect();
        let scores = score(&refs, &j.grades);
        outcomes.push(Outcome {
            judged: j,
            results,
            scores,
        });
    }

    let ndcg = mean(outcomes.iter().map(|o| o.scores.ndcg));
    let mrr = mean(outcomes.iter().filter_map(|o| o.scores.mrr));
    let recall = mean(outcomes.iter().filter_map(|o| o.scores.recall));
    let forbidden: Vec<&Outcome<'_>> = outcomes.iter().filter(|o| o.scores.forbidden > 0).collect();
    let current = Baseline {
        ndcg,
        mrr,
        recall,
        queries: outcomes.len(),
        inputs,
    };

    println!("Lodestar relevance suite");
    println!(
        "  corpus      {} torrents, {} queries judged",
        corpus.len(),
        outcomes.len()
    );
    println!("  NDCG@10     {ndcg:.4}");
    println!("  MRR         {mrr:.4}");
    println!("  recall@50   {recall:.4}");
    println!("  forbidden   {} queries", forbidden.len());

    let mut order: Vec<&Outcome<'_>> = outcomes.iter().collect();
    order.sort_by(|x, y| {
        x.scores
            .ndcg
            .total_cmp(&y.scores.ndcg)
            .then(
                x.scores
                    .mrr
                    .unwrap_or(1.0)
                    .total_cmp(&y.scores.mrr.unwrap_or(1.0)),
            )
            .then(x.judged.query.cmp(&y.judged.query))
    });
    let shown_n = if a.all {
        order.len()
    } else {
        a.worst.min(order.len())
    };
    if shown_n > 0 {
        println!(
            "\n  {} queries, worst first (results: key:grade, top {NDCG_AT})",
            if a.all { "All" } else { "Worst" }
        );
        for o in &order[..shown_n] {
            print!("{}", describe(o));
        }
    }

    let mut ok = forbidden.is_empty();
    for o in &forbidden {
        println!("  FAIL forbidden result for {:?}", o.judged.query);
    }
    if a.write_baseline {
        std::fs::write(a.dir.join("baseline.txt"), format_baseline(&current))?;
        println!("\n  baseline written: NDCG@10 {ndcg:.4}");
        return Ok(ok);
    }
    match read("baseline.txt").map(|t| parse_baseline(&t)) {
        Ok(Ok(base)) => {
            let floor = base.ndcg - a.tolerance;
            let pass = ndcg >= floor;
            println!(
                "\n  baseline    NDCG@10 {:.4} (MRR {:.4}, recall@50 {:.4}); floor {floor:.4} {}",
                base.ndcg,
                base.mrr,
                base.recall,
                if pass { "PASS" } else { "FAIL" }
            );
            ok &= pass;
            if base.inputs != current.inputs || base.queries != current.queries {
                println!(
                    "  FAIL the corpus or judgments changed since the baseline: run with --write-baseline and commit it"
                );
                ok = false;
            }
        }
        Ok(Err(e)) | Err(e) => {
            println!("\n  FAIL no usable baseline: {e}");
            ok = false;
        }
    }
    Ok(ok)
}

fn describe(o: &Outcome<'_>) -> String {
    let s = &o.scores;
    let opt = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("{v:.2}"));
    let mut out = format!(
        "  {:>5.3}  mrr {:>4}  recall {:>4}  {:?}\n         {}\n         ",
        s.ndcg,
        opt(s.mrr),
        opt(s.recall),
        o.judged.query,
        o.judged.why
    );
    if o.results.is_empty() {
        out.push_str("(no results)");
    }
    for k in o.results.iter().take(NDCG_AT) {
        let g = match o.judged.grades.get(k) {
            Some(Grade::Graded(g)) => g.to_string(),
            Some(Grade::Forbidden) => "X".into(),
            None => "-".into(),
        };
        let _ = write!(out, "{k}:{g} ");
    }
    if o.results.len() > NDCG_AT {
        let _ = write!(out, "(+{})", o.results.len() - NDCG_AT);
    }
    let missing: Vec<&str> = {
        let found: HashSet<&str> = o.results.iter().map(String::as_str).collect();
        let mut m: Vec<&str> = o
            .judged
            .grades
            .iter()
            .filter(|(k, g)| matches!(g, Grade::Graded(g) if *g > 0) && !found.contains(k.as_str()))
            .map(|(k, _)| k.as_str())
            .collect();
        m.sort_unstable();
        m
    };
    if !missing.is_empty() {
        let _ = write!(out, "\n         not found: {}", missing.join(" "));
    }
    out.push('\n');
    out
}

fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run(&a) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) if a.gate => ExitCode::FAILURE,
        Ok(false) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grades(list: &[(&str, Grade)]) -> HashMap<String, Grade> {
        list.iter().map(|(k, g)| (k.to_string(), *g)).collect()
    }

    #[test]
    fn corpus_lines_parse() {
        let text = "\
# comment
a seeders=12 | 1.5G | Some.Film.2019.1080p.mkv
b | dir | Some Pack
    700M | Disc 1/track.flac
    2K | cover.jpg
";
        let c = parse_corpus(text).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].seeders, Some(12));
        assert_eq!(c[0].files, vec![(vec![], 1_610_612_736)]);
        assert_eq!(c[1].files.len(), 2);
        assert_eq!(c[1].files[0].0, vec!["Disc 1", "track.flac"]);
        assert_eq!(c[1].files[1].1, 2048);
    }

    #[test]
    fn corpus_errors_are_caught() {
        for bad in [
            "a | 1G | x\na | 1G | y\n",
            "a | dir | x\nb | 1G | y\n",
            "a | dir | x\n",
            "  1G | stray.mkv\n",
            "a | 1G\n",
            "a | 0 | x\n",
            "a | lots | x\n",
            "a colour=red | 1G | x\n",
            "a | dir | x\n  1G | a//b\n",
        ] {
            assert!(parse_corpus(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn judgments_parse_and_validate() {
        let known: HashSet<&str> = ["a", "b", "c"].into();
        let j = parse_judgments("? q\n= why\n3 a\n1 b\nx c\n", &known).unwrap();
        assert_eq!(j[0].grades["a"], Grade::Graded(3));
        assert_eq!(j[0].grades["c"], Grade::Forbidden);
        for bad in [
            "? q\n3 a\n",
            "? q\n= why\n3 zz\n",
            "? q\n= why\n3 a\n2 a\n",
            "? q\n= why\n? q\n= again\n",
            "3 a\n",
            "? q\n= why\n= twice\n",
            "? q\n= why\n4 a\n",
        ] {
            assert!(parse_judgments(bad, &known).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn perfect_order_scores_one() {
        let g = grades(&[("a", Grade::Graded(3)), ("b", Grade::Graded(2))]);
        let s = score(&["a", "b", "z"], &g);
        assert!((s.ndcg - 1.0).abs() < 1e-12);
        assert_eq!(s.mrr, Some(1.0));
        assert_eq!(s.recall, Some(1.0));
        assert_eq!(s.forbidden, 0);
    }

    #[test]
    fn ndcg_matches_a_hand_computation() {
        let g = grades(&[("a", Grade::Graded(3)), ("b", Grade::Graded(1))]);
        // b at rank 1, a at rank 3.
        let s = score(&["b", "z", "a"], &g);
        let dcg = 1.0 / 2f64.log2() + 7.0 / 4f64.log2();
        let idcg = 7.0 / 2f64.log2() + 1.0 / 3f64.log2();
        assert!((s.ndcg - dcg / idcg).abs() < 1e-12);
        // MRR looks for grade 2+: a, at rank 3.
        assert_eq!(s.mrr, Some(1.0 / 3.0));
    }

    #[test]
    fn mrr_falls_back_to_grade_one() {
        let g = grades(&[("a", Grade::Graded(1))]);
        assert_eq!(score(&["z", "a"], &g).mrr, Some(0.5));
        assert_eq!(score(&["z"], &g).mrr, Some(0.0));
        assert_eq!(score(&["z"], &g).recall, Some(0.0));
    }

    #[test]
    fn only_the_top_ten_count_for_ndcg_and_fifty_for_recall() {
        let g = grades(&[("a", Grade::Graded(3))]);
        let mut late: Vec<String> = (0..10).map(|i| format!("z{i}")).collect();
        late.push("a".into());
        let refs: Vec<&str> = late.iter().map(String::as_str).collect();
        let s = score(&refs, &g);
        assert_eq!(s.ndcg, 0.0);
        assert_eq!(s.recall, Some(1.0));
        let mut deep: Vec<String> = (0..50).map(|i| format!("z{i}")).collect();
        deep.push("a".into());
        let refs: Vec<&str> = deep.iter().map(String::as_str).collect();
        assert_eq!(score(&refs, &g).recall, Some(0.0));
    }

    #[test]
    fn forbidden_results_are_counted_and_empty_queries_score_by_them() {
        let g = grades(&[("x", Grade::Forbidden)]);
        let clean = score(&["z"], &g);
        assert_eq!((clean.ndcg, clean.mrr, clean.forbidden), (1.0, None, 0));
        let bad = score(&["z", "x"], &g);
        assert_eq!((bad.ndcg, bad.forbidden), (0.0, 1));
        // With something to find, a forbidden result is a violation but
        // scores as grade 0.
        let g = grades(&[("a", Grade::Graded(3)), ("x", Grade::Forbidden)]);
        let s = score(&["a", "x"], &g);
        assert_eq!((s.ndcg, s.forbidden), (1.0, 1));
    }

    #[test]
    fn baseline_round_trips() {
        let b = Baseline {
            ndcg: 0.8123,
            mrr: 0.9,
            recall: 0.95,
            queries: 150,
            inputs: "00ff".into(),
        };
        assert_eq!(parse_baseline(&format_baseline(&b)).unwrap(), b);
        assert!(parse_baseline("ndcg@10 x\n").is_err());
    }

    #[test]
    fn inputs_hash_ignores_line_endings() {
        assert_eq!(
            inputs_hash("a\nb\n", "c\n"),
            inputs_hash("a\r\nb\r\n", "c\r\n")
        );
        assert_ne!(inputs_hash("a\n", "bc\n"), inputs_hash("ab\n", "c\n"));
    }

    #[test]
    fn info_dicts_parse_and_differ_by_key() {
        let e = |key: &str| Entry {
            key: key.into(),
            name: "Same.Name.mkv".into(),
            files: vec![(vec![], 5 << 20)],
            seeders: None,
        };
        let a = Info::parse(&info_dict(&e("a"))).unwrap();
        let b = Info::parse(&info_dict(&e("b"))).unwrap();
        assert_eq!(a.name.decode(), "Same.Name.mkv");
        assert_ne!(a.primary_hash(), b.primary_hash());
    }

    /// The committed corpus and judgments are well formed, and every
    /// query parses.
    #[test]
    fn committed_inputs_are_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("relevance");
        let corpus =
            parse_corpus(&std::fs::read_to_string(dir.join("corpus.txt")).unwrap()).unwrap();
        let known: HashSet<&str> = corpus.iter().map(|e| e.key.as_str()).collect();
        let judged = parse_judgments(
            &std::fs::read_to_string(dir.join("judgments.txt")).unwrap(),
            &known,
        )
        .unwrap();
        assert!(judged.len() >= 140, "{} queries", judged.len());
        for j in &judged {
            parse(&j.query).unwrap_or_else(|e| panic!("{:?}: {e}", j.query));
        }
        parse_baseline(&std::fs::read_to_string(dir.join("baseline.txt")).unwrap()).unwrap();
    }

    /// The suite end to end on a tiny corpus: hidden Adult, a negation,
    /// and the title outranking a file-only match.
    #[test]
    fn end_to_end_on_a_small_corpus() {
        let corpus = parse_corpus(
            "\
film | 2G | Lantern.Bay.2019.1080p.BluRay.x264-KSTRL
other | dir | Assorted Clips
    40M | extras/lantern bay making of.mkv
    40M | extras/other.mkv
cam | 1G | Lantern.Bay.2019.CAM.x264-KSTRL
xxx | 1G | Lantern.Bay.XXX.Parody.2020.1080p.MP4-KSTRL.mp4
",
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let (index, keys) = build(tmp.path(), &corpus).unwrap();
        let index = Arc::new(index);
        let cancel = CancelToken::new();
        let run = |q: &str| -> Vec<String> {
            let lease = Lease::new(
                index.clone(),
                &shown(parse(q).unwrap()),
                now(corpus.len()),
                Sort::Relevance,
                &cancel,
            )
            .unwrap();
            let page = lease.page(Start::Offset(0), DEPTH, &cancel).unwrap();
            page.hits.iter().map(|h| keys[&h.id].clone()).collect()
        };
        let all = run("lantern bay");
        assert!(!all.contains(&"xxx".to_string()), "{all:?}");
        let pos = |k: &str| all.iter().position(|x| x == k).unwrap();
        assert!(pos("film") < pos("other"), "{all:?}");
        assert!(!run("lantern bay -cam").contains(&"cam".to_string()));
        assert!(run("type:adult").is_empty());
    }
}
