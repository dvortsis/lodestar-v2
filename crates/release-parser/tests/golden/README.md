# Release-parser golden corpus

Hand-labelled release names (architecture §14 M3 exit criterion: ≥ 95% field
accuracy). The labels say what a careful human reads from the name. They
aren't copied from the parser's output, so a wrong parse shows up as a
miss and isn't hidden by a matching label. No third-party test suites are
copied here: guessit's data is LGPL-3.0, and anything added from another
project needs its licence checked first (§15).

## Format

```
# comment
Release.Name.2019.1080p.BluRay.x264-GROUP
  title: Release Name
  year: 2019
  res: 1080p
  source: bluray
  vcodec: h264
  group: GROUP
```

A name sits on an unindented line, and its fields are indented underneath.
A field that isn't listed must be absent from the parse. List fields are
written sorted and comma-separated, as `Release::fields` renders them.

The test scores each name over the union of its expected and parsed
fields, so a missing field and an invented field both count as misses.
Text fields (`title`, `alt`, `artist`, `album`, `author`) are compared
after folding case and punctuation, because the parser keeps the
punctuation of the original name and the corpus shouldn't depend on
whether that was `Mr. Robot` or `Mr.Robot`.

| Key | Values |
|---|---|
| `title`, `alt` (list), `artist`, `album`, `author` | text |
| `year` | `1999` or a range `1983-2016` |
| `season`, `episode`, `absolute` | `5` or a range `1-3` |
| `date` | `2024-03-15` (date-based episodes) |
| `res` | `480p`, `576p`, `720p`, `1080p`, `1080i`, `1440p`, `2160p`, `4320p` |
| `source` | `cam`, `telesync`, `telecine`, `screener`, `workprint`, `vhs`, `tv`, `dvd`, `hdrip`, `bluray`, `remux`, `web-dl`, `webrip` |
| `service` | streaming service: `amzn`, `nf`, `dsnp`, `hmax`, `max`, `atvp`, `hulu`, `pcok`, `pmtp`, `cr`, `ip`, … |
| `vcodec` | `h264`, `h265`, `av1`, `vp9`, `xvid`, `divx`, `mpeg2`, `vc1` |
| `bits` | bit depth: `8`, `10`, `12`, `16`, `24` |
| `hdr` (list) | `hdr`, `hdr10`, `hdr10+`, `dv`, `hlg` |
| `acodec` (list) | `aac`, `ac3`, `eac3`, `dts`, `dts-hd`, `dts-hd-ma`, `dts-x`, `truehd`, `atmos`, `flac`, `mp3`, `opus`, `lpcm`, `alac`, `vorbis` |
| `channels` | `1.0`, `2.0`, `5.1`, `6.1`, `7.1` |
| `lang`, `subs` (lists) | ISO 639-1 codes, plus `multi` |
| `edition` (list) | `extended`, `directors-cut`, `unrated`, `uncut`, `uncensored`, `remastered`, `theatrical`, `imax`, `criterion`, `special`, `collectors`, `anniversary`, `ultimate`, `final-cut`, `open-matte`, `deluxe`, `goty`, `definitive` |
| `flags` (list) | `proper`, `repack`, `real`, `internal`, `limited`, `3d`, `dubbed`, `hardsub`, `retail`, `portable`, `cracked`, `hybrid` |
| `pack` | `yes` (season pack, complete series, collection, discography) |
| `group` | release group, case kept |
| `container` | `mkv`, `mp4`, `avi`, `epub`, `pdf`, `iso`, … |
| `bitrate` | `320kbps`, `v0` |
| `sample` | sample rate: `44.1kHz`, `48kHz`, `96kHz`, `192kHz` |
| `version` | software version: `25.0.0.37` |
| `platform` (list) | `windows`, `macos`, `linux`, `android`, `ps4`, `switch`, `xbox360`, `pc`, … |
| `arch` | `x64`, `x86`, `arm64` |
