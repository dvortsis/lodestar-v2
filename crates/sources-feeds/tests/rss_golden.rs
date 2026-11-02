//! Golden feeds in the shapes Nyaa (RSS with `nyaa:` elements, with and
//! without `&magnets`) and AnimeTosho (RSS and Atom) serve. Written for
//! this test (made-up titles and hashes, `.example` hosts), not copied
//! from a real feed.

use lodestar_sources_feeds::rss::{Feed, Hashes, Item, Skipped, Summary, parse};

const HEX: &str = "1f2e3d4c5b6a79880123456789abcdeffedcba98";
const HEX2: &str = "00112233445566778899aabbccddeeff00112233";
const V2: &str = "a1a2a3a4a5a6a7a8a9b0b1b2b3b4b5b6b7b8b9c0c1c2c3c4c5c6c7c8c9d0d1d2";

fn v1(hex: &str) -> Hashes {
    Hashes {
        v1: Some(hex.parse().unwrap()),
        v2: None,
    }
}

fn blank(hashes: Hashes) -> Item {
    Item {
        hashes,
        title: None,
        size: None,
        published: None,
        seeders: None,
        leechers: None,
        downloads: None,
        category: None,
        trusted: None,
        remake: None,
        guid: None,
        link: None,
    }
}

fn kept(f: &Feed, n: u32) {
    assert_eq!(
        f.summary,
        Summary {
            kept: n,
            ..Summary::default()
        },
        "{f:#?}"
    );
}

#[test]
fn nyaa_rss() {
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:atom="http://www.w3.org/2005/Atom" xmlns:nyaa="https://nyaa.si/xmlns/nyaa" version="2.0">
	<channel>
		<title>Nyaa - Home - Torrent File RSS</title>
		<description>RSS Feed for Home</description>
		<link>https://nyaa.example/</link>
		<atom:link href="https://nyaa.example/?page=rss" rel="self" type="application/rss+xml" />
		<item>
			<title>[Group] Open Show - 01 (1080p) [ABCD1234].mkv</title>
				<link>https://nyaa.example/download/1000001.torrent</link>
				<guid isPermaLink="true">https://nyaa.example/view/1000001</guid>
				<pubDate>Mon, 02 Nov 2026 10:00:00 -0000</pubDate>
				<nyaa:seeders>120</nyaa:seeders>
				<nyaa:leechers>7</nyaa:leechers>
				<nyaa:downloads>3456</nyaa:downloads>
				<nyaa:infoHash>{HEX}</nyaa:infoHash>
				<nyaa:categoryId>1_2</nyaa:categoryId>
				<nyaa:category>Anime - English-translated</nyaa:category>
				<nyaa:size>1.4 GiB</nyaa:size>
				<nyaa:comments>2</nyaa:comments>
				<nyaa:trusted>Yes</nyaa:trusted>
				<nyaa:remake>No</nyaa:remake>
			<description><![CDATA[<a href="https://nyaa.example/view/1000001">#1000001 | [Group] Open Show - 01</a> | 1.4 GiB | Anime - English-translated | {HEX}]]></description>
		</item>
		<item>
			<title>Open Album &amp; Friends (FLAC)</title>
				<link>https://nyaa.example/download/1000002.torrent</link>
				<guid isPermaLink="true">https://nyaa.example/view/1000002</guid>
				<pubDate>Sun, 01 Nov 2026 23:59:59 -0000</pubDate>
				<nyaa:seeders>0</nyaa:seeders>
				<nyaa:leechers>0</nyaa:leechers>
				<nyaa:downloads>0</nyaa:downloads>
				<nyaa:infoHash>{}</nyaa:infoHash>
				<nyaa:categoryId>2_1</nyaa:categoryId>
				<nyaa:size>512.0 MiB</nyaa:size>
				<nyaa:trusted>No</nyaa:trusted>
				<nyaa:remake>Yes</nyaa:remake>
		</item>
	</channel>
</rss>"#,
        HEX2.to_uppercase()
    );
    let f = parse(body.as_bytes());
    kept(&f, 2);
    assert_eq!(
        f.items[0],
        Item {
            title: Some("[Group] Open Show - 01 (1080p) [ABCD1234].mkv".into()),
            size: Some(1_503_238_554),
            published: Some(1_793_613_600),
            seeders: Some(120),
            leechers: Some(7),
            downloads: Some(3456),
            category: Some("1_2".into()),
            trusted: Some(true),
            remake: Some(false),
            guid: Some("https://nyaa.example/view/1000001".into()),
            link: Some("https://nyaa.example/download/1000001.torrent".into()),
            ..blank(v1(HEX))
        }
    );
    let b = &f.items[1];
    assert_eq!(b.hashes, v1(HEX2));
    assert_eq!(b.title.as_deref(), Some("Open Album & Friends (FLAC)"));
    assert_eq!(b.size, Some(512 << 20));
    assert_eq!((b.trusted, b.remake), (Some(false), Some(true)));
    assert_eq!(b.category.as_deref(), Some("2_1"));
}

#[test]
fn nyaa_rss_with_magnets() {
    // `?page=rss&magnets` puts the magnet in `<link>`; the tracker URLs
    // in it are never kept.
    let body = format!(
        r#"<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa" version="2.0"><channel>
<item><title>Open Show - 02</title>
<link>magnet:?xt=urn:btih:{HEX}&amp;dn=Open%20Show&amp;tr=http%3A%2F%2Ftracker.example%2Fannounce</link>
<guid isPermaLink="true">https://nyaa.example/view/1000003</guid>
<nyaa:infoHash>{HEX}</nyaa:infoHash><nyaa:size>734 Bytes</nyaa:size>
</item></channel></rss>"#
    );
    let f = parse(body.as_bytes());
    kept(&f, 1);
    let it = &f.items[0];
    assert_eq!(it.hashes, v1(HEX));
    assert_eq!(it.link, None);
    assert_eq!(
        it.guid.as_deref(),
        Some("https://nyaa.example/view/1000003")
    );
    assert_eq!(it.size, Some(734));
}

#[test]
fn animetosho_rss() {
    // No `nyaa:` elements: the hash comes from the magnet enclosure, and
    // the enclosure's `length` (a file's size) isn't the content size.
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
<channel><title>Anime Tosho</title><link>https://animetosho.example/</link>
<atom:link href="https://feed.animetosho.example/rss2" rel="self" type="application/rss+xml"/>
<item>
<title>[Group] Open Show - 03 [1080p]</title>
<link>https://animetosho.example/view/group-open-show-03.n1000004</link>
<description><![CDATA[<strong>Total Size</strong>: 1.2 GB]]></description>
<pubDate>Mon, 02 Nov 2026 09:30:00 +0000</pubDate>
<guid isPermaLink="true">https://animetosho.example/view/group-open-show-03.n1000004</guid>
<enclosure url="magnet:?xt=urn:btih:{HEX}&amp;xt=urn:btmh:1220{V2}&amp;tr=http://t.example/a?passkey=S" type="application/x-bittorrent" length="20480"/>
</item>
<item><title>No hash here</title><link>https://animetosho.example/view/x</link></item>
</channel></rss>"#
    );
    let f = parse(body.as_bytes());
    assert_eq!(f.summary.kept, 1);
    assert_eq!(
        f.summary.skipped,
        Skipped {
            no_hash: 1,
            ..Skipped::default()
        }
    );
    let it = &f.items[0];
    assert_eq!(it.hashes.v1, Some(HEX.parse().unwrap()));
    assert_eq!(it.hashes.v2, Some(V2.parse().unwrap()));
    assert_eq!(it.size, None);
    assert_eq!(it.published, Some(1_793_611_800));
    assert_eq!(
        it.link.as_deref(),
        Some("https://animetosho.example/view/group-open-show-03.n1000004")
    );
}

#[test]
fn atom_feed() {
    // Atom in the default namespace, Nyaa's elements under another prefix.
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:n="https://nyaa.si/xmlns/nyaa">
<title>Anime Tosho</title><id>https://animetosho.example/</id>
<updated>2026-11-02T10:00:00Z</updated>
<entry>
<title type="text">[Group] Open Show - 04</title>
<id>https://animetosho.example/view/group-open-show-04.n1000005</id>
<link rel="alternate" type="text/html" href="https://animetosho.example/view/group-open-show-04.n1000005"/>
<link rel="enclosure" type="application/x-bittorrent" href="https://animetosho.example/storage/torrent/x/Open%20Show%2004.torrent"/>
<link rel="related" href="magnet:?xt=urn:btih:{HEX}&amp;dn=Open%20Show%2004"/>
<updated>2026-11-02T10:00:00Z</updated>
<published>2026-11-02T09:00:00.000+01:00</published>
<author><name>someone</name></author>
<n:seeders>5</n:seeders>
</entry>
<entry><title>Updated only</title><id>urn:uuid:42</id>
<link href="magnet:?xt=urn:btih:{HEX2}"/><updated>2026-11-02T10:00:00Z</updated></entry>
</feed>"#
    );
    let f = parse(body.as_bytes());
    kept(&f, 2);
    assert_eq!(
        f.items[0],
        Item {
            title: Some("[Group] Open Show - 04".into()),
            published: Some(1_793_606_400),
            seeders: Some(5),
            guid: Some("https://animetosho.example/view/group-open-show-04.n1000005".into()),
            link: Some("https://animetosho.example/view/group-open-show-04.n1000005".into()),
            ..blank(v1(HEX))
        }
    );
    let b = &f.items[1];
    assert_eq!(b.hashes, v1(HEX2));
    assert_eq!(b.published, Some(1_793_613_600));
    assert_eq!(b.guid.as_deref(), Some("urn:uuid:42"));
}
