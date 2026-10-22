//! The build-time half of the embedded UI (architecture §10.1, §12).
//!
//! `build.rs` includes this file with `#[path]` and calls [`bundle`] on
//! `web/build/`: every file is read, text is precompressed with brotli and
//! gzip, and each HTML page gets a Content-Security-Policy whose
//! `script-src` lists the SHA-256 of its inline scripts. The crate's tests
//! include it too, to check the same code against a fixture build.
//!
//! When there is no build (no Node in a plain `cargo test`) the bundle is a
//! one-page stub that says how to build the UI.

use std::fmt::Write as _;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// One embedded file, as `build.rs` writes it out.
#[derive(Debug)]
pub struct File {
    /// Relative to the build root, `/`-separated, no leading slash.
    pub path: String,
    pub mime: &'static str,
    /// A strong ETag for the identity body; encoded bodies add a suffix.
    pub etag: String,
    /// HTML pages only.
    pub csp: Option<String>,
    pub raw: Vec<u8>,
    pub br: Option<Vec<u8>>,
    pub gz: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct Bundle {
    /// Sorted by `path`.
    pub files: Vec<File>,
    /// False for the stub.
    pub built: bool,
}

pub const STUB: &str = "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<title>Lodestar</title>\n</head>\n<body>\n<h1>Lodestar</h1>\n\
<p>UI not built: run <code>npm run build</code> in web/, then rebuild the binary.</p>\n\
<p>The <a href=\"/status\">status page</a> works without it.</p>\n</body>\n</html>\n";

/// Bundles `dir` if it holds a build (an `index.html`), else the stub.
pub fn bundle(dir: Option<&Path>) -> io::Result<Bundle> {
    let Some(dir) = dir.filter(|d| d.join("index.html").is_file()) else {
        return Ok(Bundle {
            files: vec![file("index.html".into(), STUB.as_bytes().to_vec(), &[])?],
            built: false,
        });
    };
    let mut paths = Vec::new();
    walk(dir, String::new(), &mut paths)?;
    paths.sort();
    let mut raws = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = std::fs::read(dir.join(&path))?;
        raws.push((path, raw));
    }
    // Static `style="…"` in the components' templates, which Svelte
    // inserts as parsed HTML: each page's policy allows them by hash.
    let mut attrs = Vec::new();
    for (path, raw) in &raws {
        if path.ends_with(".js") {
            attrs.extend(js_style_attrs(&String::from_utf8_lossy(raw)));
        }
    }
    let files = raws
        .into_iter()
        .map(|(path, raw)| file(path, raw, &attrs))
        .collect::<io::Result<_>>()?;
    Ok(Bundle { files, built: true })
}

fn walk(dir: &Path, prefix: String, out: &mut Vec<String>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|n| invalid(format!("{prefix}{}: not UTF-8", n.to_string_lossy())))?;
        let path = format!("{prefix}{name}");
        // Symlinks could reach outside the build; regular files and
        // directories only.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&entry.path(), format!("{path}/"), out)?;
        } else if kind.is_file() {
            if !safe_name(&name) {
                return Err(invalid(format!("{path}: unexpected characters in name")));
            }
            out.push(path);
        }
    }
    Ok(())
}

/// The names a build can contain: content hashes are base64url, and the
/// rest are plain names. Anything else is refused, so the served paths
/// never need escaping.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@+~".contains(&b))
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn file(path: String, raw: Vec<u8>, script_attrs: &[String]) -> io::Result<File> {
    let mime = mime(&path);
    let csp = if mime.starts_with("text/html") {
        let html = std::str::from_utf8(&raw).map_err(|_| invalid(format!("{path}: not UTF-8")))?;
        Some(csp(html, script_attrs).map_err(|e| invalid(format!("{path}: {e}")))?)
    } else {
        None
    };
    let (br, gz) = if compressible(mime) {
        (smaller(brotli(&raw)?, &raw), smaller(gzip(&raw)?, &raw))
    } else {
        (None, None)
    };
    let digest = Sha256::digest(&raw);
    let mut etag = String::from("\"");
    for b in &digest[..8] {
        write!(etag, "{b:02x}").unwrap();
    }
    etag.push('"');
    Ok(File {
        path,
        mime,
        etag,
        csp,
        raw,
        br,
        gz,
    })
}

fn smaller(packed: Vec<u8>, raw: &[u8]) -> Option<Vec<u8>> {
    (packed.len() < raw.len()).then_some(packed)
}

pub fn brotli(raw: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let params = brotli::enc::BrotliEncoderParams {
        quality: 11,
        lgwin: 22,
        ..Default::default()
    };
    brotli::BrotliCompress(&mut &raw[..], &mut out, &params)?;
    Ok(out)
}

pub fn gzip(raw: &[u8]) -> io::Result<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(raw)?;
    enc.finish()
}

/// Decompresses brotli (tests check the round trip).
pub fn unbrotli(packed: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    brotli::Decompressor::new(packed, 4096).read_to_end(&mut out)?;
    Ok(out)
}

pub fn mime(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match ext.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Worth precompressing: text, and binary formats with no compression of
/// their own. Fonts and raster images are already compressed.
fn compressible(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/manifest+json"
                | "application/xml"
                | "application/wasm"
                | "image/svg+xml"
                | "image/x-icon"
        )
}

/// What an HTML page carries inline, which a strict CSP must allow by hash.
#[derive(Debug, Default, PartialEq)]
pub struct Inline {
    /// The text of each `<script>` without `src`, exactly as the browser
    /// hashes it.
    pub scripts: Vec<String>,
    /// The text of each `<style>` element.
    pub styles: Vec<String>,
    /// Each `style="…"` attribute value, entity-decoded.
    pub style_attrs: Vec<String>,
    /// `on…=` handler attributes, which this CSP never allows.
    pub handlers: Vec<String>,
}

/// Finds the inline scripts, styles and style attributes of a page. It
/// reads our own build output, so it handles what HTML serializers emit
/// (comments, quoted and unquoted attributes, raw-text elements) rather
/// than every corner of the HTML grammar.
pub fn inline(html: &str) -> Inline {
    // The HTML parser normalizes newlines before anything is hashed.
    let html = html.replace("\r\n", "\n").replace('\r', "\n");
    let b = html.as_bytes();
    let mut found = Inline::default();
    let mut i = 0;
    while let Some(off) = html[i..].find('<') {
        let lt = i + off;
        if html[lt..].starts_with("<!--") {
            i = html[lt + 4..]
                .find("-->")
                .map_or(b.len(), |e| lt + 4 + e + 3);
            continue;
        }
        let mut j = lt + 1;
        let closing = b.get(j) == Some(&b'/');
        if closing {
            j += 1;
        }
        let name_start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        if j == name_start {
            // Not a tag (`<!doctype`, a stray `<`).
            i = lt + 1;
            continue;
        }
        let name = html[name_start..j].to_ascii_lowercase();
        let (attrs, end) = attributes(&html, j);
        i = end;
        if closing {
            continue;
        }
        for (attr, value) in &attrs {
            if attr == "style" {
                found.style_attrs.push(decode_entities(value));
            } else if attr.starts_with("on") {
                found.handlers.push(attr.clone());
            }
        }
        if name == "script" || name == "style" {
            let close = format!("</{name}");
            let text_end = find_ci(&html[i..], &close).map_or(b.len(), |e| i + e);
            let text = html[i..text_end].to_string();
            if name == "style" {
                found.styles.push(text);
            } else if !attrs.iter().any(|(a, _)| a == "src") {
                found.scripts.push(text);
            }
            i = text_end;
        }
    }
    found
}

/// Parses a tag's attributes from `at` (just after its name) to the `>`;
/// returns them lowercased by name, and the index after the `>`.
fn attributes(html: &str, mut at: usize) -> (Vec<(String, String)>, usize) {
    let b = html.as_bytes();
    let mut attrs = Vec::new();
    loop {
        while at < b.len() && (b[at].is_ascii_whitespace() || b[at] == b'/') {
            at += 1;
        }
        if at >= b.len() {
            return (attrs, b.len());
        }
        if b[at] == b'>' {
            return (attrs, at + 1);
        }
        let start = at;
        while at < b.len() && !b[at].is_ascii_whitespace() && !b"=>/".contains(&b[at]) {
            at += 1;
        }
        let name = html[start..at].to_ascii_lowercase();
        while at < b.len() && b[at].is_ascii_whitespace() {
            at += 1;
        }
        let mut value = String::new();
        if b.get(at) == Some(&b'=') {
            at += 1;
            while at < b.len() && b[at].is_ascii_whitespace() {
                at += 1;
            }
            match b.get(at) {
                Some(&q @ (b'"' | b'\'')) => {
                    let end = html[at + 1..]
                        .find(q as char)
                        .map_or(b.len(), |e| at + 1 + e);
                    value = html[at + 1..end].to_string();
                    at = (end + 1).min(b.len());
                }
                _ => {
                    let start = at;
                    while at < b.len() && !b[at].is_ascii_whitespace() && b[at] != b'>' {
                        at += 1;
                    }
                    value = html[start..at].to_string();
                }
            }
        }
        if !name.is_empty() {
            attrs.push((name, value));
        }
    }
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    (0..h.len().saturating_sub(n.len() - 1)).find(|&k| h[k..k + n.len()].eq_ignore_ascii_case(n))
}

fn decode_entities(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The static `style` attribute values in compiled JavaScript: Svelte
/// templates are HTML in string literals, so `style="…"` (or `style=\"…\"`
/// inside a double-quoted string, or single quotes). Values that
/// interpolate (`${`) or hold escapes are skipped: Svelte sets dynamic
/// styles through the CSSOM, which needs no hash.
pub fn js_style_attrs(js: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = js;
    while let Some(at) = rest.find("style=") {
        rest = &rest[at + 6..];
        let close = if rest.starts_with("\\\"") {
            "\\\""
        } else if rest.starts_with('"') {
            "\""
        } else if rest.starts_with('\'') {
            "'"
        } else {
            continue;
        };
        let body = &rest[close.len()..];
        let Some(end) = body.find(close) else { break };
        let value = &body[..end];
        if !value.contains("${") && !value.contains(['\\', '"', '\'', '<', '>']) {
            found.push(decode_entities(value));
        }
        rest = &body[end + close.len()..];
    }
    found
}

/// `'sha256-…'` for a CSP source list.
pub fn hash_source(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// The Content-Security-Policy for one HTML page: only same-origin
/// resources, plus the page's own inline scripts and styles by hash.
///
/// - `style-src 'self'`: Tailwind and daisyUI ship one stylesheet, the
///   fonts are files, and Svelte sets dynamic styles through the CSSOM
///   (`style.cssText`, `setProperty`), which CSP doesn't govern.
/// - `style-src-attr` allows only the `style="…"` attributes the build
///   itself contains, by hash: the page's own (SvelteKit's
///   `<div style="display: contents">` wrapper) plus `script_attrs`, the
///   static ones in component templates (SvelteKit's visually hidden route
///   announcer), from [`js_style_attrs`].
/// - `img-src data:`: daisyUI draws checkmarks and arrows as data-URI SVG.
///
/// Refuses a page with inline event handlers rather than allow them.
pub fn csp(html: &str, script_attrs: &[String]) -> Result<String, String> {
    let mut found = inline(html);
    found.style_attrs.extend_from_slice(script_attrs);
    if let Some(h) = found.handlers.first() {
        return Err(format!(
            "inline event handler `{h}` would need 'unsafe-hashes'"
        ));
    }
    let list = |texts: &[String]| {
        let mut hashes: Vec<String> = texts.iter().map(|t| hash_source(t)).collect();
        hashes.sort();
        hashes.dedup();
        hashes.iter().fold(String::new(), |s, h| s + " " + h)
    };
    let mut policy = format!(
        "default-src 'self'; script-src 'self'{}; style-src 'self'{}; ",
        list(&found.scripts),
        list(&found.styles)
    );
    if !found.style_attrs.is_empty() {
        write!(
            policy,
            "style-src-attr 'unsafe-hashes'{}; ",
            list(&found.style_attrs)
        )
        .unwrap();
    }
    policy.push_str(
        "img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; \
         frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    );
    Ok(policy)
}

/// The Rust source `build.rs` writes: one `Asset` per file, its bodies
/// written to `out` and pulled in with `include_bytes!`.
pub fn emit(bundle: &Bundle, out: &Path) -> io::Result<String> {
    let dir = out.join("web");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut src = String::from("pub(super) static ASSETS: &[Asset] = &[\n");
    for (n, f) in bundle.files.iter().enumerate() {
        let body = |ext: &str, bytes: &Option<Vec<u8>>| -> io::Result<String> {
            let Some(bytes) = bytes else {
                return Ok("None".into());
            };
            let p = dir.join(format!("{n}.{ext}"));
            std::fs::write(&p, bytes)?;
            Ok(format!("Some(include_bytes!({:?}))", p.to_str().unwrap()))
        };
        let raw = body("raw", &Some(f.raw.clone()))?;
        writeln!(
            src,
            "    Asset {{ path: {:?}, mime: {:?}, etag: {:?}, csp: {:?}, raw: {}, br: {}, gz: {} }},",
            f.path,
            f.mime,
            f.etag,
            f.csp,
            raw.strip_prefix("Some(").unwrap().strip_suffix(')').unwrap(),
            body("br", &f.br)?,
            body("gz", &f.gz)?,
        )
        .unwrap();
    }
    src.push_str("];\n");
    writeln!(src, "pub(super) const BUILT: bool = {};", bundle.built).unwrap();
    Ok(src)
}
