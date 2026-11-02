//! Golden feeds in the shapes Prowlarr, Jackett and a Newznab indexer
//! answer with. Written for this test (open-movie titles, made-up hashes,
//! `.example` hosts, `REDACTED` keys), not copied from a real server.

use lodestar_metainfo::{InfoHash, InfoHashV2};
use lodestar_sources_feeds::torznab::{Feed, Hashes, Item, Kind, Skipped, Stop, Summary, parse};

const HEX: &str = "1f2e3d4c5b6a79880123456789abcdeffedcba98";
const B32: &str = "D4XD2TC3NJ4YQAJDIVTYTK6N577NZOUY"; // the same bytes
const HEX2: &str = "00112233445566778899aabbccddeeff00112233";
const V2: &str = "a1a2a3a4a5a6a7a8a9b0b1b2b3b4b5b6b7b8b9c0c1c2c3c4c5c6c7c8c9d0d1d2";

fn v1(hex: &str) -> Kind {
    Kind::Torrent(Hashes {
        v1: Some(hex.parse().unwrap()),
        v2: None,
    })
}

fn blank(kind: Kind) -> Item {
    Item {
        kind,
        title: None,
        size: None,
        categories: vec![],
        published: None,
        seeders: None,
        leechers: None,
        peers: None,
        files: None,
        download_factor: None,
        upload_factor: None,
        tags: vec![],
        ids: vec![],
        guid: None,
        link: None,
    }
}

/// Prowlarr's search answer: a hash attribute, a magnet-only item and
/// an item with neither.
fn prowlarr() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <atom:link rel="self" type="application/rss+xml" />
    <title>Prowlarr</title>
    <item>
      <title>Big.Buck.Bunny.2008.1080p.BluRay.x264-EXAMPLE</title>
      <description />
      <guid>https://tracker.example/torrent/1234</guid>
      <prowlarrindexer id="3" type="public">Example Public</prowlarrindexer>
      <comments>https://tracker.example/torrent/1234</comments>
      <pubDate>Sat, 31 Oct 2026 18:22:05 +0000</pubDate>
      <size>928670754</size>
      <link>http://prowlarr.example:9696/3/download?apikey=REDACTED&amp;link=Zm9vYmFy&amp;file=Big.Buck.Bunny</link>
      <category>2000</category>
      <category>2040</category>
      <category>100001</category>
      <enclosure url="http://prowlarr.example:9696/3/download?apikey=REDACTED&amp;link=Zm9vYmFy" length="928670754" type="application/x-bittorrent" />
      <torznab:attr name="rageid" value="0" />
      <torznab:attr name="imdb" value="1254207" />
      <torznab:attr name="tmdbid" value="10378" />
      <torznab:attr name="tvdbid" value="0" />
      <torznab:attr name="seeders" value="42" />
      <torznab:attr name="files" value="3" />
      <torznab:attr name="grabs" value="1200" />
      <torznab:attr name="peers" value="50" />
      <torznab:attr name="infohash" value="{HEX}" />
      <torznab:attr name="downloadvolumefactor" value="0" />
      <torznab:attr name="uploadvolumefactor" value="1" />
      <torznab:attr name="tag" value="freeleech" />
      <torznab:attr name="tag" value="internal" />
    </item>
    <item>
      <title><![CDATA[Elephants Dream (2006) [720p] & extras]]></title>
      <guid>https://tracker.example/torrent/1235</guid>
      <pubDate>Sat, 31 Oct 2026 17:00:00 +0000</pubDate>
      <size>1073741824</size>
      <category>2000</category>
      <torznab:attr name="seeders" value="5" />
      <torznab:attr name="peers" value="6" />
      <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{B32}&amp;dn=Elephants%20Dream&amp;tr=https%3A%2F%2Ftracker.example%2Fannounce%2FPASSKEY" />
    </item>
    <item>
      <title>Cosmos Laundromat (2015) 4K</title>
      <guid>https://tracker.example/torrent/1236</guid>
      <torznab:attr name="seeders" value="1" />
    </item>
  </channel>
</rss>
"#
    )
}

/// Jackett's answer: the magnet in `link`, `enclosure` and `magneturl`,
/// a numeric-entity title and a v2 hash.
fn jackett() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:torznab="http://torznab.com/schemas/2015/feed">
<channel>
<atom:link href="http://127.0.0.1:9117/api/v2.0/indexers/example/results/torznab/" rel="self" type="application/rss+xml" />
<title>Example</title>
<description>Example is a public tracker.</description>
<link>https://tracker.example/</link>
<language>en-US</language>
<category>search</category>
<item>
<title>Sintel 2010 2160p WEB-DL Pok&#233;mon-free &amp; DRM-free</title>
<guid>https://tracker.example/details/55?jackett_apikey=REDACTED</guid>
<jackettindexer id="example">Example</jackettindexer>
<type>public</type>
<comments>https://tracker.example/details/55</comments>
<pubDate>Fri, 30 Oct 2026 09:00:00 -0400</pubDate>
<size>4294967296</size>
<grabs>10</grabs>
<description />
<link>magnet:?xt=urn:btih:{HEX2}&amp;dn=Sintel</link>
<category>2045</category>
<enclosure url="magnet:?xt=urn:btih:{HEX2}&amp;dn=Sintel" length="4294967296" type="application/x-bittorrent" />
<torznab:attr name="category" value="2045" />
<torznab:attr name="category" value="100045" />
<torznab:attr name="seeders" value="7" />
<torznab:attr name="peers" value="9" />
<torznab:attr name="infohash" value="{HEX2}" />
<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{HEX2}&amp;dn=Sintel" />
<torznab:attr name="minimumratio" value="1" />
<torznab:attr name="downloadvolumefactor" value="1" />
<torznab:attr name="uploadvolumefactor" value="1" />
</item>
<item>
<title>Spring 2019 BEP52 hybrid</title>
<guid>urn:example:spring</guid>
<link>magnet:?xt=urn:btmh:1220{V2}&amp;dn=Spring</link>
<pubDate>Fri, 30 Oct 2026 08:00:00 GMT</pubDate>
<torznab:attr name="infohash" value="not-a-hash" />
</item>
<item>
<title>Spring 2019 BEP52 only</title>
<link>magnet:?xt=urn:btmh:1220{V2}&amp;dn=Spring</link>
<torznab:attr name="seeders" value="-1" />
</item>
</channel>
</rss>
"#
    )
}

/// A Newznab indexer's answer.
const NEWZNAB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
<channel>
<title>nzb.example</title>
<newznab:response offset="0" total="1" />
<item>
<title>Tears.of.Steel.2012.720p.WEB</title>
<guid isPermaLink="true">https://nzb.example/details/abcdef</guid>
<link>https://nzb.example/getnzb/abcdef.nzb&amp;i=12&amp;r=REDACTED</link>
<pubDate>Thu, 29 Oct 2026 21:15:00 +0100</pubDate>
<category>Movies &gt; HD</category>
<enclosure url="https://nzb.example/getnzb/abcdef.nzb&amp;i=12&amp;r=REDACTED" length="734003200" type="application/x-nzb" />
<newznab:attr name="category" value="2000" />
<newznab:attr name="category" value="2040" />
<newznab:attr name="size" value="734003200" />
<newznab:attr name="files" value="52" />
<newznab:attr name="poster" value="someone@example.invalid" />
<newznab:attr name="imdbid" value="tt2285752" />
</item>
</channel>
</rss>
"#;

#[test]
fn prowlarr_feed() {
    let feed = parse(prowlarr().as_bytes());
    let mut bunny = blank(v1(HEX));
    bunny.title = Some("Big.Buck.Bunny.2008.1080p.BluRay.x264-EXAMPLE".into());
    bunny.size = Some(928_670_754);
    bunny.categories = vec![2000, 2040, 100_001];
    bunny.published = Some(1_793_470_925);
    bunny.seeders = Some(42);
    bunny.peers = Some(50);
    bunny.files = Some(3);
    bunny.download_factor = Some(0.0);
    bunny.upload_factor = Some(1.0);
    bunny.tags = vec!["freeleech".into(), "internal".into()];
    bunny.ids = vec![("imdb", "tt1254207".into()), ("tmdb", "10378".into())];
    bunny.guid = Some("https://tracker.example/torrent/1234".into());
    bunny.link =
        Some("http://prowlarr.example:9696/3/download?link=Zm9vYmFy&file=Big.Buck.Bunny".into());

    let mut dream = blank(v1(HEX));
    dream.title = Some("Elephants Dream (2006) [720p] & extras".into());
    dream.size = Some(1 << 30);
    dream.categories = vec![2000];
    dream.published = Some(1_793_466_000);
    dream.seeders = Some(5);
    dream.peers = Some(6);
    dream.guid = Some("https://tracker.example/torrent/1235".into());

    assert_eq!(
        feed,
        Feed {
            items: vec![bunny, dream],
            summary: Summary {
                kept: 2,
                skipped: Skipped {
                    no_hash: 1,
                    ..Skipped::default()
                },
                stop: None,
                error: None,
            },
        }
    );
}

#[test]
fn jackett_feed() {
    let feed = parse(jackett().as_bytes());
    let mut sintel = blank(v1(HEX2));
    sintel.title = Some("Sintel 2010 2160p WEB-DL Pok\u{e9}mon-free & DRM-free".into());
    sintel.size = Some(4_294_967_296);
    sintel.categories = vec![2045, 100_045];
    sintel.published = Some(1_793_365_200);
    sintel.seeders = Some(7);
    sintel.peers = Some(9);
    sintel.download_factor = Some(1.0);
    sintel.upload_factor = Some(1.0);
    sintel.guid = Some("https://tracker.example/details/55".into());

    let v2: InfoHashV2 = V2.parse().unwrap();
    let mut spring = blank(Kind::Torrent(Hashes {
        v1: None,
        v2: Some(v2),
    }));
    spring.title = Some("Spring 2019 BEP52 only".into());

    assert_eq!(feed.items, vec![sintel, spring]);
    assert_eq!(
        feed.summary.skipped,
        Skipped {
            bad_hash: 1,
            ..Skipped::default()
        },
        "a bad infohash attribute skips the item even with a good magnet"
    );
    assert_eq!(feed.summary.stop, None);
    let Kind::Torrent(h) = feed.items[1].kind else {
        panic!()
    };
    assert_eq!(h.key(), Some(InfoHash(v2.0[..20].try_into().unwrap())));
}

#[test]
fn newznab_feed() {
    let feed = parse(NEWZNAB.as_bytes());
    let mut steel = blank(Kind::Usenet);
    steel.title = Some("Tears.of.Steel.2012.720p.WEB".into());
    steel.size = Some(734_003_200);
    steel.categories = vec![2000, 2040];
    steel.published = Some(1_793_304_900);
    steel.files = Some(52);
    steel.ids = vec![("imdb", "tt2285752".into())];
    steel.guid = Some("https://nzb.example/details/abcdef".into());
    steel.link = Some("https://nzb.example/getnzb/abcdef.nzb".into());
    assert_eq!(feed.items, vec![steel]);
    assert_eq!(feed.summary.kept, 1);
    assert_eq!(feed.summary.skipped.total(), 0);
}

#[test]
fn error_answer() {
    let body = br#"<?xml version="1.0" encoding="UTF-8"?>
<error code="100" description="Invalid API Key: REDACTED" />"#;
    let feed = parse(body);
    assert!(feed.items.is_empty());
    assert_eq!(feed.summary.error, Some(100));
    assert_eq!(feed.summary.stop, None);
}

#[test]
fn a_cut_feed_keeps_the_finished_items() {
    let full = prowlarr();
    let cut = &full[..full.find("Elephants Dream").unwrap()];
    let feed = parse(cut.as_bytes());
    assert_eq!(feed.items.len(), 1);
    assert_eq!(feed.summary.stop, Some(Stop::Malformed));
}

#[test]
fn a_hybrid_magnet_adds_v2_and_empty_fields_wait_for_real_ones() {
    let body = format!(
        r#"<rss><channel><item><title/><title>Spring</title><guid></guid><guid>urn:example:1</guid>
<torznab:attr name="infohash" value="{HEX}"/>
<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{HEX}&amp;xt=urn:btmh:1220{V2}"/></item></channel></rss>"#
    );
    let feed = parse(body.as_bytes());
    let mut want = blank(Kind::Torrent(Hashes {
        v1: Some(HEX.parse().unwrap()),
        v2: Some(V2.parse().unwrap()),
    }));
    want.title = Some("Spring".into());
    want.guid = Some("urn:example:1".into());
    assert_eq!(feed.items, vec![want]);
}
