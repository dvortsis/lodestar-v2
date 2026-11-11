//! The Newznab category map (plan §4.3 TZ-0): which Newznab categories
//! an item belongs to, from its stored class and parsed resolution, and
//! which index clauses a requested category means.
//!
//! The map, both ways:
//!
//! | Newznab | Lodestar |
//! |---|---|
//! | 2000 Movies | `type:movie` |
//! | 2030 / 2040 / 2045 Movies SD / HD / UHD | `type:movie` with `res:` 480p–576p / 720p–1440p / 2160p–4320p |
//! | 3000 Audio | `type:music`, `type:audiobook` |
//! | 3030 Audio/Audiobook | `type:audiobook` |
//! | 4000 PC | `type:software`, `type:game` |
//! | 4050 PC/Games | `type:game` |
//! | 5000 TV | `type:tv` |
//! | 5030 / 5040 / 5045 TV SD / HD / UHD | `type:tv` with `res:` as for movies |
//! | 6000 XXX | `type:adult` |
//! | 6060 XXX/ImageSet | `type:adult` with `adult:photos` |
//! | 7000 Books | `type:ebook`, `type:comic` |
//! | 7020 Books/EBook | `type:ebook` |
//! | 7030 Books/Comics | `type:comic` |
//! | 8000 Other, 8010 Other/Misc | `type:images`, `type:video`, `type:other`, and anything not classified yet |
//!
//! An item is listed under its parent and, when one applies, its
//! subcategory; asking for a parent finds every item of its
//! subcategories. A movie or show whose resolution wasn't parsed is in
//! 2000 or 5000 only, so asking for 2040 doesn't find it: a client gets
//! what it would label the item as, no more.

use lodestar_classify::{AdultKind, ContentType, Record};
use lodestar_search::Node;
use lodestar_search::query::TagKey;

/// One Newznab category this server lists in its caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    pub id: u32,
    pub name: &'static str,
    /// Its subcategories (empty for a subcategory).
    pub subs: &'static [Category],
}

const fn sub(id: u32, name: &'static str) -> Category {
    Category { id, name, subs: &[] }
}

/// Every category served, in caps order.
pub const CATEGORIES: &[Category] = &[
    Category {
        id: MOVIES,
        name: "Movies",
        subs: &[
            sub(MOVIES + SD, "Movies/SD"),
            sub(MOVIES + HD, "Movies/HD"),
            sub(MOVIES + UHD, "Movies/UHD"),
        ],
    },
    Category {
        id: AUDIO,
        name: "Audio",
        subs: &[sub(AUDIOBOOK, "Audio/Audiobook")],
    },
    Category {
        id: PC,
        name: "PC",
        subs: &[sub(PC_GAMES, "PC/Games")],
    },
    Category {
        id: TV,
        name: "TV",
        subs: &[
            sub(TV + SD, "TV/SD"),
            sub(TV + HD, "TV/HD"),
            sub(TV + UHD, "TV/UHD"),
        ],
    },
    Category {
        id: XXX,
        name: "XXX",
        subs: &[sub(XXX_IMAGESET, "XXX/ImageSet")],
    },
    Category {
        id: BOOKS,
        name: "Books",
        subs: &[sub(EBOOK, "Books/EBook"), sub(COMICS, "Books/Comics")],
    },
    Category {
        id: OTHER,
        name: "Other",
        subs: &[sub(MISC, "Other/Misc")],
    },
];

pub const MOVIES: u32 = 2000;
pub const AUDIO: u32 = 3000;
pub const AUDIOBOOK: u32 = 3030;
pub const PC: u32 = 4000;
pub const PC_GAMES: u32 = 4050;
pub const TV: u32 = 5000;
pub const XXX: u32 = 6000;
pub const XXX_IMAGESET: u32 = 6060;
pub const BOOKS: u32 = 7000;
pub const EBOOK: u32 = 7020;
pub const COMICS: u32 = 7030;
pub const OTHER: u32 = 8000;
pub const MISC: u32 = 8010;
/// Offsets of the resolution subcategories under Movies and TV.
const SD: u32 = 30;
const HD: u32 = 40;
const UHD: u32 = 45;

/// The `res:` values of each resolution band, as the index stores them.
const SD_RES: &[&str] = &["480p", "576p"];
const HD_RES: &[&str] = &["720p", "1080i", "1080p", "1440p"];
const UHD_RES: &[&str] = &["2160p", "4320p"];

/// The types that list under Other: everything without a closer home.
const OTHER_TYPES: &[&str] = &["images", "video", "other"];

/// The categories an item lists under: its parent first, then its
/// subcategory when one applies. `record` is its stored classification;
/// `None` (not classified yet, or unreadable) is Other.
pub fn of(record: Option<&Record>) -> (u32, Option<u32>) {
    match record {
        None => (OTHER, Some(MISC)),
        Some(r) => {
            let res: Vec<&str> = r.release_all("res").flat_map(|v| v.split(", ")).collect();
            of_parts(r.content_type, r.adult_kind, &res)
        }
    }
}

/// [`of`], from a class, its adult kind and its `res` values.
pub fn of_parts(t: ContentType, kind: Option<AdultKind>, res: &[&str]) -> (u32, Option<u32>) {
    let band = || {
        res.iter()
            .find_map(|v| band_of(&v.to_ascii_lowercase()))
    };
    match t {
        ContentType::Movie => (MOVIES, band().map(|b| MOVIES + b)),
        ContentType::Tv => (TV, band().map(|b| TV + b)),
        ContentType::Music => (AUDIO, None),
        ContentType::Audiobook => (AUDIO, Some(AUDIOBOOK)),
        ContentType::Ebook => (BOOKS, Some(EBOOK)),
        ContentType::Comic => (BOOKS, Some(COMICS)),
        ContentType::Software => (PC, None),
        ContentType::Game => (PC, Some(PC_GAMES)),
        ContentType::Images | ContentType::Video | ContentType::Other => (OTHER, Some(MISC)),
        ContentType::Adult => (
            XXX,
            (kind == Some(AdultKind::Photos)).then_some(XXX_IMAGESET),
        ),
    }
}

/// The resolution band offset of a `res:` value.
fn band_of(res: &str) -> Option<u32> {
    if SD_RES.contains(&res) {
        Some(SD)
    } else if HD_RES.contains(&res) {
        Some(HD)
    } else if UHD_RES.contains(&res) {
        Some(UHD)
    } else {
        None
    }
}

fn types(any: &[&str]) -> Node {
    Node::Tag {
        key: TagKey::Type,
        any: any.iter().map(|t| t.to_string()).collect(),
    }
}

fn res(any: &[&str]) -> Node {
    Node::Tag {
        key: TagKey::Res,
        any: any.iter().map(|t| t.to_string()).collect(),
    }
}

/// The index clause a requested category means; `None` for one this
/// server doesn't list (which finds nothing).
pub fn clause(id: u32) -> Option<Node> {
    let both = |a: Node, b: Node| Node::And { of: vec![a, b] };
    Some(match id {
        MOVIES => types(&["movie"]),
        TV => types(&["tv"]),
        x if x == MOVIES + SD => both(types(&["movie"]), res(SD_RES)),
        x if x == MOVIES + HD => both(types(&["movie"]), res(HD_RES)),
        x if x == MOVIES + UHD => both(types(&["movie"]), res(UHD_RES)),
        x if x == TV + SD => both(types(&["tv"]), res(SD_RES)),
        x if x == TV + HD => both(types(&["tv"]), res(HD_RES)),
        x if x == TV + UHD => both(types(&["tv"]), res(UHD_RES)),
        AUDIO => types(&["music", "audiobook"]),
        AUDIOBOOK => types(&["audiobook"]),
        PC => types(&["software", "game"]),
        PC_GAMES => types(&["game"]),
        XXX => types(&["adult"]),
        XXX_IMAGESET => both(
            types(&["adult"]),
            Node::Tag {
                key: TagKey::Adult,
                any: vec![AdultKind::Photos.as_str().to_string()],
            },
        ),
        BOOKS => types(&["ebook", "comic"]),
        EBOOK => types(&["ebook"]),
        COMICS => types(&["comic"]),
        OTHER | MISC => Node::Or {
            of: vec![
                types(OTHER_TYPES),
                // Not classified yet: no `type:` at all.
                Node::Not {
                    of: Box::new(Node::Tag {
                        key: TagKey::Type,
                        any: ContentType::ALL
                            .iter()
                            .map(|t| t.as_str().to_string())
                            .collect(),
                    }),
                },
            ],
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ids() -> Vec<u32> {
        CATEGORIES
            .iter()
            .flat_map(|c| std::iter::once(c.id).chain(c.subs.iter().map(|s| s.id)))
            .collect()
    }

    #[test]
    fn every_listed_category_has_a_clause_and_nothing_else_does() {
        for id in all_ids() {
            assert!(clause(id).is_some(), "{id}");
        }
        for id in [0, 1000, 2010, 2050, 5070, 6010, 9000, u32::MAX] {
            assert!(clause(id).is_none(), "{id}");
        }
    }

    #[test]
    fn subcategories_sit_under_their_parent() {
        for c in CATEGORIES {
            assert_eq!(c.id % 1000, 0);
            for s in c.subs {
                assert_eq!(s.id / 1000 * 1000, c.id, "{}", s.name);
                assert!(s.name.starts_with(c.name), "{}", s.name);
            }
        }
    }

    #[test]
    fn every_type_lists_under_a_served_category() {
        let ids = all_ids();
        for t in ContentType::ALL {
            let (parent, sub) = of_parts(t, None, &[]);
            assert!(ids.contains(&parent), "{t:?}");
            if let Some(s) = sub {
                assert!(ids.contains(&s), "{t:?}");
                assert_eq!(s / 1000 * 1000, parent, "{t:?}");
            }
        }
        assert_eq!(of(None), (OTHER, Some(MISC)));
    }

    #[test]
    fn resolution_picks_the_band() {
        for (res, want) in [
            ("480p", Some(2030)),
            ("1080p", Some(2040)),
            ("1080i", Some(2040)),
            ("2160P", Some(2045)),
            ("weird", None),
        ] {
            assert_eq!(
                of_parts(ContentType::Movie, None, &[res]),
                (MOVIES, want),
                "{res}"
            );
        }
        assert_eq!(of_parts(ContentType::Tv, None, &["720p"]), (TV, Some(5040)));
        assert_eq!(of_parts(ContentType::Music, None, &["720p"]), (AUDIO, None));
    }

    #[test]
    fn adult_photos_are_an_image_set() {
        assert_eq!(of_parts(ContentType::Adult, None, &[]), (XXX, None));
        assert_eq!(
            of_parts(ContentType::Adult, Some(AdultKind::Photos), &[]),
            (XXX, Some(XXX_IMAGESET))
        );
    }
}
