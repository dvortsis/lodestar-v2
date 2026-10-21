//! Markers → fields, then title and group from what's left.

use crate::lexicon::{self, Hit, Strength};
use crate::token::{Tok, Tokens, tokenize};
use crate::{
    AudioCodec, Bitrate, Channels, Date, Edition, Flag, MAX_INPUT_BYTES, Release, Resolution,
    Source, Span,
};

/// Plausible release years. The upper bound keeps `Cyberpunk 2077` and
/// `Blade Runner 2049`-style titles out without consulting a clock, which
/// would make parsing depend on when it ran.
const YEARS: std::ops::RangeInclusive<u32> = 1890..=2039;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Year,
    /// Seasons, episodes and air dates.
    Episode,
    Tech,
}

#[derive(Debug)]
struct Mark {
    /// Word range `start..end`.
    start: usize,
    end: usize,
    strength: Strength,
    kind: Kind,
    hits: Vec<Hit>,
}

/// Parses a torrent, folder or file name. Never panics; see the crate
/// docs for how.
pub fn parse(name: &str) -> Release {
    let name = truncate(name, MAX_INPUT_BYTES);
    let (body, container) = strip_extension(name.trim());
    let tokens = tokenize(body);
    let mut release = Parser::new(&tokens).run();
    if release.container.is_none() {
        release.container = container;
    }
    release
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Removes known file extensions (`.mkv`, and a `.part` around another
/// one) and returns the first real one as the container.
fn strip_extension(name: &str) -> (&str, Option<&'static str>) {
    let mut body = name;
    for _ in 0..2 {
        let Some(dot) = body.rfind('.') else { break };
        let ext = &body[dot + 1..];
        let rest = body[..dot].trim_end();
        if rest.is_empty() || ext.len() > 5 {
            break;
        }
        let low = ext.to_ascii_lowercase();
        if low == "part" {
            body = rest;
            continue;
        }
        if let Some(c) = lexicon::extension(&low) {
            return (rest, Some(c));
        }
        break;
    }
    (body, None)
}

struct Parser<'t, 'a> {
    t: &'t Tokens<'a>,
    marks: Vec<Mark>,
    /// The mark each word belongs to.
    owner: Vec<Option<usize>>,
    /// First word that can belong to the title (after a site prefix or a
    /// leading `[Group]`).
    start: usize,
    leading: Option<usize>,
    /// Most letters are capitals, so ALL CAPS says nothing.
    caps_name: bool,
}

impl<'t, 'a> Parser<'t, 'a> {
    fn new(t: &'t Tokens<'a>) -> Self {
        let (upper, letters) = t
            .text
            .chars()
            .filter(char::is_ascii_alphabetic)
            .fold((0usize, 0usize), |(u, l), c| {
                (u + usize::from(c.is_ascii_uppercase()), l + 1)
            });
        Parser {
            t,
            marks: Vec::new(),
            owner: vec![None; t.toks.len()],
            start: 0,
            leading: None,
            caps_name: letters > 0 && upper * 10 >= letters * 7,
        }
    }

    fn toks(&self) -> &[Tok<'a>] {
        &self.t.toks
    }

    fn tok(&self, i: usize) -> Option<&Tok<'a>> {
        self.t.toks.get(i)
    }

    fn run(mut self) -> Release {
        self.skip_prefix();
        let mut i = self.start;
        while i < self.toks().len() {
            match self.match_at(i) {
                Some(m) => {
                    let idx = self.marks.len();
                    for o in &mut self.owner[m.start..m.end] {
                        *o = Some(idx);
                    }
                    i = m.end;
                    self.marks.push(m);
                }
                None => i += 1,
            }
        }
        if let Some(b) = self.leading {
            let br = &self.t.brackets[b];
            if self.owner[br.first..br.end].iter().any(Option::is_some) {
                self.leading = None;
            }
        }
        if let Some(r) = self.scene_music() {
            return r;
        }
        let mut title_end = self.title_end();
        let group = if let Some(b) = self.leading {
            let br = &self.t.brackets[b];
            Some(self.t.text[br.inner_from..br.inner_to].trim().to_owned())
        } else if let Some((k, g)) = self.dash_group(title_end) {
            title_end = title_end.min(k);
            Some(g)
        } else {
            self.bracket_group(title_end)
        };
        let mut r = Release {
            group,
            ..Release::default()
        };
        let mut dash_episode = None;
        for m in self.marks.iter().filter(|m| m.start >= title_end) {
            for &hit in &m.hits {
                self.apply(&mut r, hit, &mut dash_episode);
            }
        }
        let (title, mut dangling_dash) = self.title_text(self.start, title_end);
        // `Metallica - Discography`: the dash is the next marker's separator.
        dangling_dash |= self.tok(title_end).is_some_and(Tok::spaced_dash);
        self.finish(&mut r, title_end, title, dangling_dash, dash_episode);
        r
    }

    /// Skips a leading `www.site.com -` or `[site.com]`, and notes a
    /// leading `[Group]`.
    fn skip_prefix(&mut self) {
        let toks = self.toks();
        if let Some(b) = self
            .t
            .brackets
            .first()
            .filter(|b| b.at_start && b.first == 0)
        {
            if b.end > b.first && b.end < toks.len() {
                let site = (b.first + 1..b.end)
                    .any(|j| toks[j].sep == "." && lexicon::is_tld(&toks[j].low));
                if !site {
                    self.leading = Some(0);
                }
                self.start = b.end;
            }
            return;
        }
        if toks.first().is_some_and(|t| t.low.ends_with("www")) {
            if let Some(j) = (1..toks.len().min(5))
                .find(|&j| toks[j].sep == "." && lexicon::is_tld(&toks[j].low))
            {
                if j + 1 < toks.len() {
                    self.start = j + 1;
                }
            }
        }
    }

    fn in_bracket(&self, i: usize) -> bool {
        self.toks()[i].bracket.is_some()
    }

    /// The word opens a bracket group: `(1080p)` for `1080p`.
    fn opens_bracket(&self, i: usize) -> bool {
        self.toks()[i]
            .bracket
            .is_some_and(|b| self.t.brackets[b].first == i)
    }

    fn all_caps(&self, from: usize, to: usize) -> bool {
        let mut letters = 0;
        for t in &self.toks()[from..to] {
            for c in t.text.chars().filter(|c| c.is_alphabetic()) {
                if !c.is_uppercase() {
                    return false;
                }
                letters += 1;
            }
        }
        letters >= 2
    }

    fn is_strong(&self, m: &Mark) -> bool {
        m.strength == Strength::Strong
            || self.in_bracket(m.start)
            || (!self.caps_name && self.all_caps(m.start, m.end))
    }

    // ---- Markers -----------------------------------------------------

    fn match_at(&self, i: usize) -> Option<Mark> {
        self.crc(i)
            .or_else(|| self.date(i))
            .or_else(|| self.season_episode(i))
            .or_else(|| self.dash_episode(i))
            .or_else(|| self.year(i))
            .or_else(|| self.resolution(i))
            .or_else(|| self.glued_channels(i))
            .or_else(|| self.version(i))
            .or_else(|| self.audio_numbers(i))
            .or_else(|| self.language(i))
            .or_else(|| self.phrase(i))
            .or_else(|| self.word(i))
            .or_else(|| self.bare_channels(i))
    }

    fn mark(
        start: usize,
        end: usize,
        strength: Strength,
        kind: Kind,
        hits: Vec<Hit>,
    ) -> Option<Mark> {
        Some(Mark {
            start,
            end,
            strength,
            kind,
            hits,
        })
    }

    fn tech(start: usize, end: usize, strength: Strength, hits: Vec<Hit>) -> Option<Mark> {
        Self::mark(start, end, strength, Kind::Tech, hits)
    }

    /// `[A1B2C3D4]`: a CRC32 in brackets on its own.
    fn crc(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let b = &self.t.brackets[t.bracket?];
        let hex = t.low.len() == 8
            && t.low.bytes().all(|c| c.is_ascii_hexdigit())
            && t.low.bytes().any(|c| c.is_ascii_digit());
        (b.first == i && b.end == i + 1 && hex).then_some(())?;
        Self::tech(i, i + 1, Strength::Strong, vec![Hit::Noise])
    }

    /// `2024.03.15`, `2024-03-15` or `15.03.2024`.
    fn date(&self, i: usize) -> Option<Mark> {
        let (a, b, c) = (self.tok(i)?, self.tok(i + 1)?, self.tok(i + 2)?);
        if !(b.sep == c.sep && matches!(b.sep, "." | "-" | " ")) {
            return None;
        }
        let num = |t: &Tok<'_>, len: usize| (t.low.len() == len).then(|| t.number()).flatten();
        let (y, m, d) = match (num(a, 4), num(c, 4)) {
            (Some(y), _) => (y, num(b, 2)?, num(c, 2)?),
            (None, Some(y)) => (y, num(b, 2)?, num(a, 2)?),
            _ => return None,
        };
        (YEARS.contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d)).then_some(())?;
        let date = Date {
            year: y as u16,
            month: m as u8,
            day: d as u8,
        };
        Self::mark(
            i,
            i + 3,
            Strength::Strong,
            Kind::Episode,
            vec![Hit::Date(date)],
        )
    }

    fn season_episode(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let low = t.low.as_str();
        let strong = Strength::Strong;
        let ep = Kind::Episode;

        // S01, S01E02, S01E02E03, then ranges in the next word.
        if let Some((season, rest)) = lead_num(low, "s", 4) {
            if rest.is_empty() {
                let next = self.tok(i + 1);
                if let Some(n) = next.filter(|n| n.tight_dash()) {
                    if let Some(last) = lead_num(&n.low, "s", 4)
                        .filter(|(_, r)| r.is_empty())
                        .map(|(s, _)| s)
                        .or_else(|| n.number().filter(|_| n.low.len() <= 2))
                    {
                        let span = span(season, last)?;
                        return Self::mark(i, i + 2, strong, ep, vec![Hit::Season(span)]);
                    }
                }
                if let Some(n) = next.filter(|n| n.dot_or_space()) {
                    if let Some((e, "")) = lead_num(&n.low, "e", 4) {
                        let (end, last) = self.episode_range(i + 2, e);
                        return Self::mark(
                            i,
                            end,
                            strong,
                            ep,
                            vec![
                                Hit::Season(Span::one(season as u16)),
                                Hit::Episode(span(e, last)?),
                            ],
                        );
                    }
                }
                return Self::mark(
                    i,
                    i + 1,
                    strong,
                    ep,
                    vec![Hit::Season(Span::one(season as u16))],
                );
            }
            if let Some((first, mut rest)) = lead_num(rest, "e", 4) {
                let mut last = first;
                while let Some((e, r)) = lead_num(rest, "e", 4) {
                    last = e;
                    rest = r;
                }
                if !rest.is_empty() {
                    return None;
                }
                let (end, last) = self.episode_range(i + 1, last);
                return Self::mark(
                    i,
                    end,
                    strong,
                    ep,
                    vec![
                        Hit::Season(Span::one(season as u16)),
                        Hit::Episode(span(first, last)?),
                    ],
                );
            }
            return None;
        }

        // 1x02
        if let Some((s, rest)) = lead_num(low, "", 2) {
            if let Some((e, "")) = lead_num(rest, "x", 3) {
                return Self::mark(
                    i,
                    i + 1,
                    strong,
                    ep,
                    vec![
                        Hit::Season(Span::one(s as u16)),
                        Hit::Episode(Span::one(e as u16)),
                    ],
                );
            }
        }

        // E05, Ep05
        if let Some((e, "")) = lead_num(low, "e", 4).or_else(|| lead_num(low, "ep", 4)) {
            let (end, last) = self.episode_range(i + 1, e);
            return Self::mark(i, end, strong, ep, vec![Hit::Episode(span(e, last)?)]);
        }

        // 第5集, 第2季
        if let Some(rest) = low.strip_prefix('第') {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let n: u32 = digits.parse().ok().filter(|&n| n > 0 && n < 10_000)?;
            let hit = match &rest[digits.len()..] {
                "集" | "話" | "话" => Hit::Episode(Span::one(n as u16)),
                "季" => Hit::Season(Span::one(n as u16)),
                _ => return None,
            };
            return Self::mark(i, i + 1, strong, ep, vec![hit]);
        }

        // Season 1, Season 1-9, 2nd Season, Episode 3, Серии 1-6.
        let season_word = matches!(
            low,
            "season"
                | "seasons"
                | "saison"
                | "saisons"
                | "temporada"
                | "stagione"
                | "staffel"
                | "сезон"
                | "сезоны"
        );
        let episode_word = matches!(
            low,
            "episode" | "episodes" | "ep" | "eps" | "серии" | "серия"
        );
        if season_word || episode_word {
            let n = self.tok(i + 1)?;
            let first = n.number().filter(|&v| v > 0 && v < 10_000)?;
            let (end, last) = self.number_range(i + 2, first);
            let span = span(first, last)?;
            let hit = if season_word {
                Hit::Season(span)
            } else {
                Hit::Episode(span)
            };
            return Self::mark(i, end, strong, ep, vec![hit]);
        }
        if let Some(n) = ordinal(low) {
            if self.tok(i + 1).is_some_and(|t| t.low == "season") {
                return Self::mark(i, i + 2, strong, ep, vec![Hit::Season(Span::one(n as u16))]);
            }
        }
        None
    }

    /// After an episode number: `-E03` or `-03` extends it to a range.
    fn episode_range(&self, i: usize, first: u32) -> (usize, u32) {
        if let Some(n) = self.tok(i).filter(|n| n.tight_dash()) {
            if let Some((e, "")) = lead_num(&n.low, "e", 4) {
                return (i + 1, e);
            }
            if let Some(e) = n.number().filter(|&e| n.low.len() <= 3 && e > first) {
                return (i + 1, e);
            }
        }
        (i, first)
    }

    /// `N-M`, `N ~ M` or `N to M` after a number.
    fn number_range(&self, i: usize, first: u32) -> (usize, u32) {
        if let Some(n) = self.tok(i) {
            let joined = n.sep.trim() == "-" || n.sep.trim() == "~";
            if joined {
                if let Some(last) = n.number().filter(|&l| l > first && l < 10_000) {
                    return (i + 1, last);
                }
            }
        }
        (i, first)
    }

    /// Anime numbering: `Title - 05`, `Title - 01v2`, `Title - 01-12`,
    /// `Title - 01 ~ 28`, and a bare zero-padded `01-26`. Only when the
    /// number is followed by a bracket, a ` - ` or the end, and the name
    /// looks like anime (a leading `[Group]`, a zero-padded number or a
    /// range), so `Adele - 30 (2021)` stays an album title.
    fn dash_episode(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let (num, suffix) = lead_num(&t.low, "", 4)?;
        if !(suffix.is_empty() || (suffix.len() == 2 && suffix.starts_with('v'))) {
            return None;
        }
        if t.low.len() == 4 && YEARS.contains(&num) {
            return None;
        }
        let padded = t.low.starts_with('0') && t.low.len() > 1;
        let spaced = t.spaced_dash() || (t.sep.contains('-') && t.sep.ends_with(' '));
        if !(spaced || padded) {
            return None;
        }
        let (end, last) = self.number_range(i + 1, num);
        let ranged = end > i + 1;
        let followed =
            end == self.toks().len() || self.opens_bracket(end) || self.toks()[end].spaced_dash();
        let anime = self.leading.is_some() || padded || ranged;
        if !(followed && anime && (spaced || (padded && ranged))) {
            return None;
        }
        Self::mark(
            i,
            end,
            Strength::Strong,
            Kind::Episode,
            vec![Hit::DashEpisode(span(num, last)?)],
        )
    }

    /// `1999`, `(2019)`, `1983-2016`.
    fn year(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let y = t
            .number()
            .filter(|y| t.low.len() == 4 && YEARS.contains(y))?;
        if let Some(n) = self.tok(i + 1).filter(|n| n.tight_dash()) {
            if let Some(last) = n
                .number()
                .filter(|l| n.low.len() == 4 && YEARS.contains(l) && *l > y)
            {
                return Self::mark(
                    i,
                    i + 2,
                    Strength::Strong,
                    Kind::Year,
                    vec![Hit::Year(span(y, last)?)],
                );
            }
        }
        Self::mark(
            i,
            i + 1,
            Strength::Strong,
            Kind::Year,
            vec![Hit::Year(Span::one(y as u16))],
        )
    }

    /// `1080p`, `720i`, `4K`, `1920x1080`, `BD1080p`.
    fn resolution(&self, i: usize) -> Option<Mark> {
        let low = self.tok(i)?.low.as_str();
        let mut hits = Vec::new();
        let rest = match low.strip_prefix("bd") {
            Some(r) if r.ends_with('p') => {
                hits.push(Hit::Source(Source::BluRay));
                r
            }
            _ => low,
        };
        let res = match rest {
            "4k" => Some(Resolution::R2160p),
            "8k" => Some(Resolution::R4320p),
            _ => match lead_num(rest, "", 4) {
                Some((h, "p")) => progressive(h),
                Some((1080, "i")) => Some(Resolution::R1080i),
                Some((w, r)) => lead_num(r, "x", 4)
                    .filter(|(h, tail)| tail.is_empty() && w >= 320 && *h >= 240)
                    .map(|(h, _)| from_height(h)),
                None => None,
            },
        }?;
        hits.push(Hit::Res(res));
        Self::tech(i, i + 1, Strength::Strong, hits)
    }

    /// `DDP5.1`, `AAC2.0`, `DD+5.1`, `DDP5 1`, `AAC51`.
    fn glued_channels(&self, i: usize) -> Option<Mark> {
        let low = self.tok(i)?.low.as_str();
        let digits_at = low.find(|c: char| c.is_ascii_digit())?;
        let codec = lexicon::channel_prefix(&low[..digits_at])?;
        let digits = &low[digits_at..];
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (ch, end) = match digits.as_bytes() {
            [a] => {
                let n = self.tok(i + 1).filter(|n| n.dot_or_space())?;
                match n.low.as_bytes() {
                    [b @ (b'0' | b'1')] => ((a - b'0', b - b'0'), i + 2),
                    _ => return None,
                }
            }
            [a, b @ (b'0' | b'1')] => ((a - b'0', b - b'0'), i + 1),
            _ => return None,
        };
        matches!(ch.0, 1 | 2 | 5 | 6 | 7).then_some(())?;
        Self::tech(
            i,
            end,
            Strength::Strong,
            vec![Hit::Audio(codec), Hit::Channels(ch.0, ch.1)],
        )
    }

    /// `5.1`, `7.1`, `2.0` on their own: weak, since `Tron 2.0` is a title.
    fn bare_channels(&self, i: usize) -> Option<Mark> {
        let (a, b) = (self.tok(i)?, self.tok(i + 1)?);
        let ok = b.sep == "."
            && matches!(a.low.as_str(), "1" | "2" | "5" | "6" | "7")
            && matches!(b.low.as_str(), "0" | "1")
            && !self
                .tok(i + 2)
                .is_some_and(|c| c.sep == "." && c.is_digits());
        ok.then_some(())?;
        let ch = Hit::Channels(a.low.parse().ok()?, b.low.parse().ok()?);
        Self::tech(i, i + 2, Strength::Weak, vec![ch])
    }

    /// `v1.2.3`, `1.2.3`, `24.04`, `v7.01`; `V0` is a bitrate.
    fn version(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let (prefixed, head) = match t.low.strip_prefix('v') {
            Some(d) if !d.is_empty() && d.len() <= 8 && d.bytes().all(|b| b.is_ascii_digit()) => {
                (true, d)
            }
            _ if t.is_digits() && t.low.len() <= 6 => (false, t.low.as_str()),
            _ => return None,
        };
        let mut end = i + 1;
        while self
            .tok(end)
            .is_some_and(|n| n.sep == "." && n.is_digits() && n.low.len() <= 8)
        {
            end += 1;
        }
        let parts = end - i;
        if !prefixed {
            let year_like = head.len() == 4 && head.parse().is_ok_and(|y: u32| YEARS.contains(&y));
            let channels_like = parts == 2 && head.len() == 1 && self.toks()[i + 1].low.len() == 1;
            // `Fast.and.Furious.6.2013`: a sequel number, then the year.
            let then_year = self.toks()[i + 1..end]
                .iter()
                .any(|p| p.low.len() == 4 && p.number().is_some_and(|y| YEARS.contains(&y)));
            if parts < 2 || year_like || channels_like || then_year {
                return None;
            }
        } else if head == "0" && parts == 1 {
            return None;
        }
        let from = t.from + usize::from(prefixed);
        let to = self.toks()[end - 1].to;
        Self::tech(
            i,
            end,
            Strength::Strong,
            vec![Hit::Version(from as u32, to as u32)],
        )
    }

    /// Bit depth, sample rate, bitrate and sizes: `10bit`, `10-bit`,
    /// `24-96`, `96kHz`, `44.1kHz`, `320kbps`, `999MB`, `MULTi13`, `75th
    /// Anniversary Edition`.
    fn audio_numbers(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let low = t.low.as_str();
        let next = self.tok(i + 1);
        let strong = Strength::Strong;

        // 10bit, 24Bit, 64bit (an architecture).
        let bits_word = |s: &str| matches!(s, "bit" | "bits" | "bpc");
        let bits = match lead_num(low, "", 2) {
            Some((n, rest)) if bits_word(rest) => Some((n, i + 1)),
            Some((n, "")) if next.is_some_and(|x| bits_word(&x.low) && !x.sep.contains(' ')) => {
                Some((n, i + 2))
            }
            _ => None,
        };
        if let Some((n, end)) = bits {
            let hit = match n {
                8 | 10 | 12 | 16 | 24 => Hit::Bits(n as u8),
                32 => Hit::Arch("x86"),
                64 => Hit::Arch("x64"),
                _ => return None,
            };
            return Self::tech(i, end, strong, vec![hit]);
        }

        // 24-96, 16-44.
        if let (Some(depth @ (16 | 24 | 32)), Some(n)) =
            (t.number(), next.filter(|n| n.tight_dash()))
        {
            if let Some(hz) = n.number().and_then(sample_rate) {
                return Self::tech(
                    i,
                    i + 2,
                    strong,
                    vec![Hit::Bits(depth as u8), Hit::Sample(hz)],
                );
            }
        }

        // 96kHz, 44.1kHz, 48 kHz.
        if let Some((k, rest)) = lead_num(low, "", 3) {
            if rest == "khz" {
                return Self::tech(i, i + 1, strong, vec![Hit::Sample(sample_rate(k)?)]);
            }
            if rest.is_empty() {
                if let Some(n) = next {
                    if n.sep == "." && matches!(n.low.as_str(), "1khz" | "2khz" | "4khz") {
                        return Self::tech(i, i + 2, strong, vec![Hit::Sample(sample_rate(k)?)]);
                    }
                    if n.low == "khz" {
                        return Self::tech(i, i + 2, strong, vec![Hit::Sample(sample_rate(k)?)]);
                    }
                    if n.low == "kbps" && (32..=1411).contains(&k) {
                        return Self::tech(
                            i,
                            i + 2,
                            strong,
                            vec![Hit::Bitrate(Bitrate::Kbps(k as u16))],
                        );
                    }
                }
            }
            if rest == "kbps" && (32..=1411).contains(&k) {
                return Self::tech(
                    i,
                    i + 1,
                    strong,
                    vec![Hit::Bitrate(Bitrate::Kbps(k as u16))],
                );
            }
        }

        // Sizes: 999MB, 4.37GB.
        if let Some((_, unit)) = lead_num(low, "", 5) {
            if matches!(unit, "mb" | "gb" | "tb" | "mib" | "gib") {
                return Self::tech(i, i + 1, Strength::Weak, vec![Hit::Noise]);
            }
        }

        // MULTi13: a multi-language release with 13 languages.
        if let Some(n) = low.strip_prefix("multi") {
            if !n.is_empty() && n.len() <= 2 && n.bytes().all(|b| b.is_ascii_digit()) {
                return Self::tech(i, i + 1, strong, vec![Hit::Lang("multi")]);
            }
        }

        // 75th Anniversary (Edition).
        if ordinal(low).is_some() && next.is_some_and(|n| n.low == "anniversary") {
            let end = if self.tok(i + 2).is_some_and(|n| n.low == "edition") {
                i + 3
            } else {
                i + 2
            };
            return Self::tech(i, end, strong, vec![Hit::Edition(Edition::Anniversary)]);
        }
        None
    }

    /// A language, and what follows it: `GERMAN.DL` (dual language),
    /// `Eng-Subs`, `English Dub`, `Sub ITA`.
    fn language(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let next = self.tok(i + 1);
        let subs_word = |s: &str| {
            matches!(
                s,
                "sub" | "subs" | "subbed" | "subtitle" | "subtitles" | "subtitled"
            )
        };
        if subs_word(&t.low) {
            let code = next.and_then(|n| lexicon::lang(&n.low))?;
            return Self::tech(i, i + 2, Strength::Strong, vec![Hit::Subs(code)]);
        }
        let code = lexicon::lang(&t.low)?;
        let after = next.map(|n| n.low.as_str());
        match after {
            Some(s) if subs_word(s) => {
                Self::tech(i, i + 2, Strength::Strong, vec![Hit::Subs(code)])
            }
            Some("dub" | "dubbed") => Self::tech(
                i,
                i + 2,
                Strength::Strong,
                vec![Hit::Lang(code), Hit::Flag(Flag::Dubbed)],
            ),
            Some("dl") => Self::tech(
                i,
                i + 2,
                Strength::Weak,
                vec![Hit::Lang(code), Hit::Lang("multi")],
            ),
            _ => None,
        }
    }

    fn phrase(&self, i: usize) -> Option<Mark> {
        let mut best: Option<(usize, Strength, &'static [Hit])> = None;
        for &(words, strength, hits) in lexicon::PHRASES {
            let fits = words.len() > best.map_or(0, |b| b.0)
                && words
                    .iter()
                    .enumerate()
                    .all(|(k, w)| self.tok(i + k).is_some_and(|t| t.low == *w));
            if fits {
                best = Some((words.len(), strength, hits));
            }
        }
        let (len, strength, hits) = best?;
        Self::tech(i, i + len, strength, hits.to_vec())
    }

    fn word(&self, i: usize) -> Option<Mark> {
        let t = self.tok(i)?;
        let (strength, mut hits) = match lexicon::word(&t.low, self.all_caps(i, i + 1)) {
            Some((strength, hits)) => (strength, hits.to_vec()),
            None => (Strength::Weak, vec![Hit::Lang(lexicon::lang(&t.low)?)]),
        };
        let mut end = i + 1;
        // MP3 320: a bare bitrate after the codec.
        if matches!(hits[..], [Hit::Audio(AudioCodec::Mp3 | AudioCodec::Aac)]) {
            if let Some(k) = self
                .tok(i + 1)
                .filter(|n| n.dot_or_space())
                .and_then(Tok::number)
                .filter(|k| matches!(k, 64 | 96 | 128 | 160 | 192 | 224 | 256 | 320))
            {
                hits.push(Hit::Bitrate(Bitrate::Kbps(k as u16)));
                end += 1;
            }
        }
        Self::tech(i, end, strength, hits)
    }

    // ---- Title and group ---------------------------------------------

    /// Where the title stops: the first strong marker, moved back over
    /// weak markers directly before it (`Movie.FRENCH.DVDRip`).
    fn title_end(&self) -> usize {
        let n = self.toks().len();
        let marks: Vec<&Mark> = self
            .marks
            .iter()
            .filter(|m| m.start >= self.start)
            .collect();
        let mut end = n;
        let mut ended_by_year = false;
        for (k, m) in marks.iter().enumerate() {
            if !self.is_strong(m) {
                continue;
            }
            if m.kind == Kind::Year {
                // A year that starts the title belongs to it (`1917`,
                // `2001 A Space Odyssey`).
                if m.start == self.start {
                    continue;
                }
                // Of two years in a row, the first is the title's
                // (`Blade Runner 2049 2017`).
                if marks
                    .get(k + 1)
                    .is_some_and(|x| x.kind == Kind::Year && x.start == m.end)
                {
                    continue;
                }
                // A bare year followed by a bracketed one, with nothing
                // strong between (`Taylor Swift - 1989 (Taylor's Version)
                // (2023)`).
                if !self.in_bracket(m.start) {
                    let later = marks[k + 1..]
                        .iter()
                        .take_while(|x| x.kind == Kind::Year || !self.is_strong(x))
                        .any(|x| x.kind == Kind::Year && self.in_bracket(x.start));
                    if later {
                        continue;
                    }
                }
            }
            end = m.start;
            ended_by_year = m.kind == Kind::Year;
            break;
        }
        if end < n && !ended_by_year {
            // Noise (`Update`, sizes) doesn't pull the title back.
            while let Some(prev) = self
                .marks
                .iter()
                .find(|p| p.end == end && p.start > self.start && p.hits != [Hit::Noise])
            {
                end = prev.start;
            }
        }
        end
    }

    /// `-GROUP` at the end (before trailing brackets), or ` - GROUP` after
    /// a quality marker (`x264 - YIFY`). Returns where it starts.
    fn dash_group(&self, title_end: usize) -> Option<(usize, String)> {
        let toks = self.toks();
        let tail = self.trailing_brackets_start();
        if tail <= self.start + 1 {
            return None;
        }
        let k = (self.start + 1..tail)
            .rev()
            .find(|&k| toks[k].tight_dash() || toks[k].spaced_dash())?;
        let segment = &toks[k..tail];
        let ok = segment.len() <= 3
            && segment[1..].iter().all(|t| t.sep == "_")
            && self.owner[k..tail].iter().all(Option::is_none)
            && !segment.iter().all(Tok::is_digits);
        if !ok {
            return None;
        }
        // The marker before the group must be metadata after the title,
        // not a weak word inside it (`Fleetwood Mac - Rumours`).
        let prev_mark = self.owner[k - 1]
            .map(|m| &self.marks[m])
            .filter(|m| m.start >= title_end);
        let accepted = if toks[k].spaced_dash() {
            prev_mark.is_some_and(|m| m.kind == Kind::Tech)
        } else {
            // With no marker before it, only a dotted scene name
            // (`Doom.Eternal-CODEX`) has a group; `Spider-Man` doesn't.
            prev_mark.is_some() || (title_end == toks.len() && self.dotted(self.start, k))
        };
        let text = &self.t.text[toks[k].from..toks[tail - 1].to];
        accepted.then(|| (k, text.to_owned()))
    }

    /// Words `from..to` are joined by dots, scene style.
    fn dotted(&self, from: usize, to: usize) -> bool {
        to > from + 1
            && self.toks()[from + 1..to]
                .iter()
                .all(|t| t.sep == "." || t.sep == "-")
    }

    /// The first word of the run of bracket groups that ends the name.
    fn trailing_brackets_start(&self) -> usize {
        let mut tail = self.toks().len();
        for b in self.t.brackets.iter().rev() {
            if b.first == b.end {
                continue;
            }
            if b.end != tail {
                break;
            }
            tail = b.first;
        }
        tail
    }

    /// A trailing bracket that isn't metadata: `[YTS.MX]`, `[FitGirl
    /// Repack]`, `(Zone-Empire)`.
    fn bracket_group(&self, title_end: usize) -> Option<String> {
        let mut tail = self.toks().len();
        for b in self.t.brackets.iter().rev() {
            if b.first == b.end {
                continue;
            }
            if b.end != tail || b.first < title_end.max(self.start) {
                return None;
            }
            let free: Vec<usize> = (b.first..b.end)
                .filter(|&j| self.owner[j].is_none())
                .collect();
            // Only pure noise (`[ABCD1234]`, `[USA]`) is looked past; a
            // metadata bracket ends the search.
            let noise_only = free.is_empty()
                && (b.first..b.end)
                    .filter_map(|j| self.owner[j])
                    .all(|m| self.marks[m].hits == [Hit::Noise]);
            if free.is_empty() && !noise_only {
                return None;
            }
            if let (Some(&first), Some(&last)) = (free.first(), free.last()) {
                let toks = self.toks();
                // Beside metadata, a group is one word (`[FitGirl Repack]`);
                // more is a description (`[Vinyl Rip 24-96]`).
                let mixed = free.len() < b.end - b.first;
                if free.iter().all(|&j| toks[j].is_digits()) || (mixed && free.len() > 1) {
                    return None;
                }
                return Some(self.t.text[toks[first].from..toks[last].to].to_owned());
            }
            tail = b.first;
        }
        None
    }

    /// The title text of words `from..to`. Dotted names become spaced
    /// (keeping acronyms like `S.H.I.E.L.D.`); spaced names keep their
    /// punctuation. Also says whether a dangling ` -` was trimmed, which
    /// means the title was cut before something after a dash
    /// (`Metallica - Discography`).
    fn title_text(&self, from: usize, to: usize) -> (String, bool) {
        if from >= to {
            return (String::new(), false);
        }
        let toks = self.toks();
        let mut end = toks[to - 1].to;
        // Keep symbols glued to the last word: `Notepad++`.
        if let Some(next) = toks.get(to) {
            end += next.sep.bytes().take_while(|&b| b == b'+').count();
        }
        let slice = &self.t.text[toks[from].from..end];
        let mut out = String::with_capacity(slice.len());
        if slice.contains(char::is_whitespace) {
            for c in slice.chars() {
                let c = if c == '_' { ' ' } else { c };
                if !(c == ' ' && out.ends_with(' ')) {
                    out.push(c);
                }
            }
        } else {
            out.push_str(toks[from].text);
            for j in from + 1..to {
                let acronym =
                    toks[j].sep == "." && toks[j].is_single_char() && toks[j - 1].is_single_char();
                if acronym {
                    out.push('.');
                } else {
                    for c in toks[j].sep.chars() {
                        let c = if c == '.' || c == '_' { ' ' } else { c };
                        if !(c == ' ' && out.ends_with(' ')) {
                            out.push(c);
                        }
                    }
                }
                out.push_str(toks[j].text);
            }
            let last = to - 1;
            if last > from
                && toks[last].is_single_char()
                && toks[last].sep == "."
                && toks[last - 1].is_single_char()
            {
                out.push('.');
            }
            out.push_str(&slice[toks[to - 1].to - toks[from].from..]);
        }
        tidy_title(out)
    }

    fn scene_music(&self) -> Option<Release> {
        // Artist_Name-Album-(Tag)-WEB-2024-GROUP: no spaces or dots, and
        // dashes separate the fields.
        let toks = self.toks();
        if self.start != 0
            || self
                .t
                .text
                .contains(|c: char| c.is_whitespace() || c == '.')
            || toks.len() < 4
        {
            return None;
        }
        let boundary = |t: &Tok<'_>| t.sep.contains('-');
        let fields: Vec<usize> = (0..toks.len())
            .filter(|&j| j == 0 || boundary(&toks[j]))
            .collect();
        if fields.len() < 4 {
            return None;
        }
        let has_year = self
            .marks
            .iter()
            .any(|m| m.kind == Kind::Year && fields.contains(&m.start));
        let free = |from: usize, to: usize| self.owner[from..to].iter().all(Option::is_none);
        if !has_year
            || !free(fields[0], fields[1])
            || !free(fields[1], fields[2])
            || self.marks.iter().any(|m| m.kind == Kind::Episode)
        {
            return None;
        }
        let last = *fields.last()?;
        let mut r = Release::default();
        if last + 1 == toks.len() && self.owner[last].is_none() {
            r.group = Some(toks[last].text.to_owned());
        }
        let mut dash = None;
        for m in self.marks.iter().filter(|m| m.start >= fields[2]) {
            for &hit in &m.hits {
                self.apply(&mut r, hit, &mut dash);
            }
        }
        let field =
            |a: usize, b: usize| self.t.text[toks[a].from..toks[b - 1].to].replace('_', " ");
        let artist = field(fields[0], fields[1]);
        let album = field(fields[1], fields[2]);
        r.title = format!("{artist} - {album}");
        r.artist = Some(artist);
        r.album = Some(album);
        Some(r)
    }

    // ---- Fields ------------------------------------------------------

    fn apply(&self, r: &mut Release, hit: Hit, dash_episode: &mut Option<Span>) {
        fn once<T>(slot: &mut Option<T>, v: T) {
            if slot.is_none() {
                *slot = Some(v);
            }
        }
        fn add<T: PartialEq>(list: &mut Vec<T>, v: T) {
            if !list.contains(&v) {
                list.push(v);
            }
        }
        match hit {
            Hit::Res(x) => once(&mut r.resolution, x),
            Hit::Uhd => {}
            Hit::Source(s) => match r.source {
                Some(Source::BluRay) if s == Source::Remux => r.source = Some(s),
                _ => once(&mut r.source, s),
            },
            Hit::Service(s) => once(&mut r.service, s),
            Hit::VCodec(c) => once(&mut r.video_codec, c),
            Hit::Bits(b) => once(&mut r.bit_depth, b),
            Hit::Hdr(h) => add(&mut r.hdr, h),
            Hit::Audio(a) => add(&mut r.audio, a),
            Hit::Channels(a, b) => once(&mut r.channels, Channels(a, b)),
            Hit::Lang(l) => add(&mut r.languages, l),
            Hit::Subs(l) => add(&mut r.subtitles, l),
            Hit::Edition(e) => add(&mut r.editions, e),
            Hit::Flag(f) => add(&mut r.flags, f),
            Hit::Pack => r.pack = true,
            Hit::Container(c) => once(&mut r.container, c),
            Hit::Platform(p) => add(&mut r.platforms, p),
            Hit::Arch(a) => add(&mut r.arch, a),
            Hit::Bitrate(b) => once(&mut r.bitrate, b),
            Hit::Sample(s) => once(&mut r.sample_rate, s),
            Hit::Book | Hit::Noise => {}
            Hit::Year(y) => once(&mut r.year, y),
            Hit::Season(s) => once(&mut r.season, s),
            Hit::Episode(e) => once(&mut r.episode, e),
            Hit::DashEpisode(e) => once(dash_episode, e),
            Hit::Date(d) => once(&mut r.date, d),
            Hit::Version(from, to) => {
                once(
                    &mut r.version,
                    self.t.text[from as usize..to as usize].to_owned(),
                );
            }
        }
    }

    fn finish(
        &self,
        r: &mut Release,
        title_end: usize,
        title: String,
        dangling_dash: bool,
        dash_episode: Option<Span>,
    ) {
        let accepted = |h: Hit| {
            self.marks
                .iter()
                .any(|m| m.start >= title_end && m.hits.contains(&h))
        };
        if let Some(e) = dash_episode {
            if r.episode.is_none() {
                r.episode = Some(e);
            } else {
                r.absolute = Some(e);
            }
        }
        if r.season.is_some() && r.episode.is_none() && r.date.is_none() {
            r.pack = true;
        }
        if r.resolution.is_none() && accepted(Hit::Uhd) {
            r.resolution = Some(Resolution::R2160p);
        }
        // `MP3 V2` is a LAME preset, not a version.
        if r.audio.contains(&AudioCodec::Mp3) {
            if let Some(v @ ("1" | "2")) = r.version.as_deref() {
                r.bitrate = Some(Bitrate::Vbr(v.parse().unwrap_or(0)));
                r.version = None;
            }
        }

        let mut parts = split_alt(&title);
        r.title = if parts.is_empty() {
            String::new()
        } else {
            parts.remove(0)
        };
        r.alt_titles = parts;

        let video = r.video_codec.is_some() || r.resolution.is_some();
        let episodic = r.season.is_some() || r.episode.is_some() || r.date.is_some();
        let book_format = matches!(
            r.container,
            Some("epub" | "mobi" | "azw3" | "azw" | "pdf" | "djvu" | "fb2" | "lit")
        );
        let book = !video && !episodic && (book_format || accepted(Hit::Book));
        let music_codec = r.audio.iter().any(|a| {
            matches!(
                a,
                AudioCodec::Flac | AudioCodec::Mp3 | AudioCodec::Alac | AudioCodec::Vorbis
            )
        });
        let music_file = matches!(
            r.container,
            Some("mp3" | "flac" | "m4a" | "ogg" | "opus" | "wav" | "ape" | "wv" | "dsf" | "dff")
        );
        let music = !book
            && !video
            && !episodic
            && (music_codec || music_file || r.bitrate.is_some() || r.sample_rate.is_some());

        if book {
            if let Some((t, author)) = split_word(&r.title, " by ") {
                r.author = Some(author);
                r.title = t;
            } else if let Some((author, _)) = split_word(&r.title, " - ") {
                r.author = Some(author);
            }
        } else if music {
            if let Some((artist, album)) = split_word(&r.title, " - ") {
                r.artist = Some(artist);
                if !is_pack_word(&album) {
                    r.album = Some(album);
                }
            } else if dangling_dash && !r.title.is_empty() {
                r.artist = Some(r.title.clone());
            }
        }
    }
}

/// Trims separators and dangling brackets off a title and closes an
/// unclosed parenthesis (`Requiem (Karajan, 1976)` cut at the year).
fn tidy_title(mut s: String) -> (String, bool) {
    let mut dangling_dash = false;
    loop {
        let trimmed = s.trim_end_matches(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ':' | ';' | '/' | '|' | '~' | '_')
        });
        let trimmed = if let Some(t) = trimmed.strip_suffix(['-', '–', '—']) {
            dangling_dash = true;
            t
        } else {
            trimmed.trim_end_matches(['(', '[', '{', '（', '【'])
        };
        if trimmed.len() == s.len() {
            break;
        }
        s.truncate(trimmed.len());
    }
    let lead = s.len()
        - s.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '-' | '_' | '.' | ',' | ':' | '~')
        })
        .len();
    s.drain(..lead);
    // Close the one bracket a cut at a marker left open; more than one
    // unclosed means the name itself is unbalanced, and stays as written.
    for (open, close) in [('(', ')'), ('[', ']')] {
        if s.matches(open).count() == s.matches(close).count() + 1 {
            s.push(close);
        }
    }
    (s, dangling_dash)
}

/// `Мастер и Маргарита / The Master and Margarita` → both titles.
fn split_alt(title: &str) -> Vec<String> {
    let mut parts = vec![title.to_owned()];
    for sep in [" / ", " | ", " aka ", " AKA ", " a.k.a. "] {
        parts = parts
            .iter()
            .flat_map(|p| p.split(sep).map(str::to_owned).collect::<Vec<_>>())
            .collect();
    }
    parts
        .into_iter()
        .map(|p| tidy_title(p).0)
        .filter(|p| !p.is_empty())
        .collect()
}

/// Splits at the first `sep` (ASCII, any case), if both sides have text.
fn split_word(s: &str, sep: &str) -> Option<(String, String)> {
    let at = s
        .as_bytes()
        .windows(sep.len())
        .position(|w| w.eq_ignore_ascii_case(sep.as_bytes()))?;
    let (a, b) = (s[..at].trim(), s[at + sep.len()..].trim());
    (!a.is_empty() && !b.is_empty()).then(|| (a.to_owned(), b.to_owned()))
}

fn is_pack_word(s: &str) -> bool {
    matches!(
        s.to_lowercase().as_str(),
        "discography" | "discografia" | "anthology" | "collection" | "complete"
    )
}

/// `prefix` then 1..=`max` digits: `("s01e02", "s")` → `(1, "e02")`.
fn lead_num<'s>(s: &'s str, prefix: &str, max: usize) -> Option<(u32, &'s str)> {
    let rest = s.strip_prefix(prefix)?;
    let n = rest.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || n > max {
        return None;
    }
    Some((rest[..n].parse().ok()?, &rest[n..]))
}

fn span(first: u32, last: u32) -> Option<Span> {
    let first = u16::try_from(first).ok()?;
    let last = u16::try_from(last).ok()?;
    (first <= last).then_some(Span { first, last })
}

/// `1st`, `2nd`, `3rd`, `75th`.
fn ordinal(low: &str) -> Option<u32> {
    let (n, rest) = lead_num(low, "", 3)?;
    matches!(rest, "st" | "nd" | "rd" | "th").then_some(n)
}

fn progressive(h: u32) -> Option<Resolution> {
    Some(match h {
        240 | 360 | 480 => Resolution::R480p,
        540 | 576 => Resolution::R576p,
        720 => Resolution::R720p,
        1080 => Resolution::R1080p,
        1440 => Resolution::R1440p,
        2160 => Resolution::R2160p,
        4320 => Resolution::R4320p,
        _ => return None,
    })
}

fn from_height(h: u32) -> Resolution {
    match h {
        4000.. => Resolution::R4320p,
        2000.. => Resolution::R2160p,
        1400.. => Resolution::R1440p,
        1000.. => Resolution::R1080p,
        700.. => Resolution::R720p,
        560.. => Resolution::R576p,
        _ => Resolution::R480p,
    }
}

/// kHz as written (`44`, `96`) → Hz.
fn sample_rate(k: u32) -> Option<u32> {
    Some(match k {
        44 => 44_100,
        88 => 88_200,
        176 => 176_400,
        22 => 22_050,
        32 | 48 | 96 | 192 | 384 => k * 1000,
        _ => return None,
    })
}
