#![allow(dead_code)]
pub const HIDDEN: &str = "[hidden]";
pub struct Url;
mod m;
mod p;
use std::panic::{catch_unwind, AssertUnwindSafe};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[self.below(v.len())]
    }
}

const TOK: &[&str] = &[
    "://", "@", "/", ":", "?", "#", "[", "]", " ", "\"", "'", ")", ",", "}", "<", ">", "`",
    "magnet:", "magnet:?xt=", "http://", "https://h/", "http://a@h/", "path: \"", "username: \"",
    "password: Some(\"", "fragment: Some(\"", "query: Some(\"", "Url { scheme: \"https\", ", "%2F",
    "%3A", "%3D", "=", "&", ";", "token=", "a", "0", "é", "ü", "[redacted]", "[::1]", ":80", "\n",
    "\t", "udp://t.example:80/announce,", "x", "passkey", "user:", "Some(", "\\", "\\\"", "+", "~",
    "_", "-", ".", "Bearer ", "%40", "mailto:",
];
const SYN: &[&str] = &[
    ":", "/", "@", "?", "#", ",", "'", ")", "[", "]", " ", "\"", "<", ">", "`", "://", "%2F", "%40",
    "\\", "é", "=", "&", ";", "}", "\n", "%3A", "+", "\t",
];
const SCH: &[&str] = &[
    "http", "https", "udp", "ftp", "ws", "wss", "socks5", "x", "HTTP", "git+ssh", "s3",
];

fn junk(r: &mut Rng, max: usize, set: &[&str]) -> String {
    let n = r.below(max + 1);
    (0..n).map(|_| r.pick(set)).collect()
}

fn run(input: &str, f: fn(&str) -> String) -> Result<String, ()> {
    catch_unwind(AssertUnwindSafe(|| f(input))).map_err(|_| ())
}

const LIMIT: usize = 100000;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("one") {
        let t = &args[2];
        println!("main={:?}", m::scrub_text(t));
        println!("pr  ={:?}", p::scrub_text(t));
        println!("main abs={:?}", m::scrub(t, m::Form::Absolute));
        println!("pr   abs={:?}", p::scrub(t, p::Form::Absolute));
        println!("main req={:?}", m::scrub(t, m::Form::Request));
        println!("pr   req={:?}", p::scrub(t, p::Form::Request));
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("file") {
        for line in std::fs::read_to_string(&args[2]).unwrap().lines() {
            if line.is_empty() {
                continue;
            }
            let t = line.replace("\\n", "\n").replace("\\t", "\t");
            println!("== {t:?}");
            println!("  main={:?}", m::scrub_text(&t));
            println!("  pr  ={:?}", p::scrub_text(&t));
            println!("  main abs={:?}", m::scrub(&t, m::Form::Absolute));
            println!("  pr   abs={:?}", p::scrub(&t, p::Form::Absolute));
        }
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("times") {
        for unit in std::fs::read_to_string(&args[2]).unwrap().lines() {
            if unit.is_empty() {
                continue;
            }
            let unit = &unit.replace("\\n", "\n"); let mut row = format!("{unit:?}:");
            for mb in [1usize, 4] {
                let text = unit.repeat((mb << 20) / unit.len());
                let t = std::time::Instant::now();
                std::hint::black_box(p::scrub_text(&text));
                let tp = t.elapsed();
                let t = std::time::Instant::now();
                std::hint::black_box(m::scrub_text(&text));
                let tm = t.elapsed();
                row.push_str(&format!(" {mb}MiB pr={tp:?} main={tm:?};"));
            }
            println!("{row}");
        }
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("time") {
        let unit = &args[2];
        let mb: usize = args[3].parse().unwrap();
        let text = unit.repeat((mb << 20) / unit.len());
        let t = std::time::Instant::now();
        let o = p::scrub_text(&text);
        println!("pr {unit:?} {mb} MiB: {:?} (out {} bytes)", t.elapsed(), o.len());
        let t = std::time::Instant::now();
        let o = m::scrub_text(&text);
        println!("main {unit:?} {mb} MiB: {:?} (out {} bytes)", t.elapsed(), o.len());
        return;
    }
    let seed: u64 = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(7);
    let n: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(1_000_000);
    std::panic::set_hook(Box::new(|_| {}));
    let mut r = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let (mut reg, mut pshow, mut mshow, mut ppanic, mut mpanic) = (0, 0, 0, 0, 0);
    let mut printed = 0;
    for _ in 0..n {
        let s = format!("Zq{}9", ["S3cr3t", "aB0", "k3yV4l", "Xy7Xy7Xy"][r.below(4)]);
        let pk = format!("Zq{0}90{0}x", ["0123456789", "abcdef0123"][r.below(2)]);
        let sch = r.pick(SCH);
        let pre = junk(&mut r, 6, TOK);
        let post = junk(&mut r, 6, TOK);
        let a = junk(&mut r, 3, SYN);
        let b = junk(&mut r, 3, SYN);
        let mid = junk(&mut r, 2, TOK);
        let host = [
            "host.example",
            "[::1]",
            "[::1]:6969",
            "h:80",
            "h",
            "10.0.0.1:8080",
            "h.example:abc",
        ][r.below(7)];
        let case = r.below(16);
        let (body, sec): (String, &str) = match case {
            0..=2 => (format!("{sch}://u:{a}{s}{b}@{host}/x"), &s),
            3 => (format!("{sch}://{a}{s}{b}@{host}/x"), &s),
            4 => (format!("{sch}://{host}/a?token={s}&b=1"), &s),
            5 => (format!("{sch}://{host}/ann/{pk}/announce"), &pk),
            6 => (format!("{sch}://{host}/dl/{pk}.torrent"), &pk),
            7 => (format!("Url {{ scheme: \"{sch}\", cannot_be_a_base: false, username: \"u{a}\", password: Some(\"{a}{s}{b}\"), host: Some(Domain(\"h\")), port: None, path: \"/x\", query: None, fragment: None }}"), &s),
            8 => (format!("Url {{ scheme: \"{sch}\", username: \"\", password: None, host: Some(Domain(\"h\")), port: None, path: \"/ann/{pk}/announce\", query: Some(\"passkey={s}\"), fragment: Some(\"{a}{s}\") }}"), &s),
            9 => (format!("magnet:?xt=urn:btih:abc&tr={sch}://u:{a}{s}@{host}/announce"), &s),
            10 => (format!("{sch}://u:{a}{s}{b}@{host}/x{mid}{sch}://v:{a}w@h2/"), &s),
            11 => (format!("{sch}://h/x{mid}{sch}://u:{a}{s}{b}@{host}/x"), &s),
            12 => (format!("{sch}://{host}/x?{a}passkey={s}#{b}"), &s),
            13 => (format!("{sch}://u:{a}{s}{b}@{host}:80/x?y=1#f"), &s),
            14 => (format!("token={s}"), &s),
            _ => (format!("{sch}://u%3A{a}{s}{b}@{host}/x"), &s),
        };
        let mut text = format!("{pre}{body}{post}");
        if std::env::var("NO_NL").is_ok() { text = text.replace('\n', " "); }
        if std::env::var("NO_DBG").is_ok() && (text.contains("Some(") || text.contains(": \"")) { continue; }
        if std::env::var("NO_MAG").is_ok() && text.to_ascii_lowercase().contains("magnet:") { continue; }
        let pm = run(&text, m::scrub_text);
        let pp = run(&text, p::scrub_text);
        if pm.is_err() {
            mpanic += 1;
        }
        if pp.is_err() {
            ppanic += 1;
            if printed < LIMIT {
                println!("PR PANIC: {text:?}");
                printed += 1;
            }
        }
        let mh = pm.as_ref().map_or(true, |o| !o.contains(sec));
        let ph = pp.as_ref().map_or(true, |o| !o.contains(sec));
        if !mh {
            mshow += 1;
        }
        if !ph {
            pshow += 1;
        }
        if mh && !ph && pm.is_ok() {
            reg += 1;
            if printed < LIMIT {
                println!(
                    "REG case={case} a={a:?} b={b:?} mid={mid:?} host={host:?}: {text:?}\n  main={:?}\n  pr  ={:?}",
                    pm.unwrap(),
                    pp.unwrap()
                );
                printed += 1;
            }
        }
        for form in 0..2 {
            let (fm, fp) = if form == 0 {
                (m::Form::Absolute, p::Form::Absolute)
            } else {
                (m::Form::Request, p::Form::Request)
            };
            let om = catch_unwind(AssertUnwindSafe(|| m::scrub(&text, fm)));
            let op = catch_unwind(AssertUnwindSafe(|| p::scrub(&text, fp)));
            if op.is_err() {
                ppanic += 1;
                if printed < LIMIT {
                    println!("PR scrub PANIC: {text:?}");
                    printed += 1;
                }
            }
            if let (Ok(om), Ok(op)) = (om, op) {
                if !om.contains(sec) && op.contains(sec) {
                    reg += 1;
                    if printed < LIMIT {
                        println!("REG scrub form{form}: {text:?}\n  main={om:?}\n  pr  ={op:?}");
                        printed += 1;
                    }
                }
            }
        }
    }
    println!("seed={seed} n={n} regressions={reg} pr_shown={pshow} main_shown={mshow} pr_panics={ppanic} main_panics={mpanic}");
}
