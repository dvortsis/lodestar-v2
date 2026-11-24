# Announce definitions

A definition tells Lodestar how to read one private tracker's IRC announce channel: which network and channels to join, which nicks post announces, how their lines read, and where the `.torrent` lives. The format is Lodestar's own (plan `docs/plan/2026-11-05-v07-m7.md`, Q7). `crates/sources-irc/src/announce.rs` parses it; [`template.def`](template.def) is a complete, working example for a fictional tracker.

## Rules for adding a definition
- **Write it from the tracker's own published announce format.** Copy nothing from another project's definitions (autobrr's included): their licence compatibility with Lodestar's AGPL-3.0 is a legal question for the owner (plan Q7).
- **Only for a tracker whose published rules allow announce bots** (owner-decisions §2.47, plan Q2). Record the rules page you read, and the date, in the PR's decisions file.
- **Never put a passkey, RSS key or any personal link in a definition.** The URL template takes `{passkey}`; Lodestar fills it in only when you ask for a file, and never stores or logs the filled URL. A template with what looks like a pasted passkey outside a placeholder (20 or more letters and digits in a row, or 32 or more hex digits in a run that may also hold `-`, as a UUID does, in the path or in the host) is refused.

No tracker definitions ship yet. IRC-2, the plugin that uses them, adds the first ones under the rules above.

## Format
A UTF-8 text file of at most 16 KiB (a byte-order mark at the start is ignored). Each line is blank, a comment starting with `#`, or `key = value`. Spaces around the key and value are ignored, and a value runs to the end of the line. Lines are at most 2,048 bytes. Values can't hold control characters (a tab included), invisible Unicode format characters, or the line and paragraph separators U+2028 and U+2029.

| Key | Times | Value |
|---|---|---|
| `lodestar-announce` | first line, once | the format version, `1` |
| `id` | once | lower-case letters, digits and `-`, at most 32 bytes, not starting with `-` |
| `name` | once | the display name, at most 64 bytes |
| `network` | once | the IRC server's host name |
| `channel` | 1 to 8 | a channel name (`#`, `&`, `+` or `!`, then 1 to 49 more bytes; no space, comma or `:`) |
| `announcer` | 1 to 8 | an RFC 2812 nick (letters, digits and ``[]\`_^{|}-``, at most 30) |
| `announcer-host` | 0 to 8 | the host an announcer's messages come from (the part after `@` in `nick!user@host`: a host name, address or the network's cloak such as `user/announcer`), matched exactly, ignoring case (no wildcards): at most 253 printable ASCII bytes with no `!`, `@` or `*` |
| `pattern` | 1 to 4 | a regular expression (Rust `regex` syntax), at most 1,024 bytes |
| `torrent-url` | 0 or 1 | an `https://` URL template, at most 512 bytes |

### Patterns
- Named captures: **`name`** (required), `size`, `category` and `id`. Any other named capture is refused; unnamed groups are fine.
- Each pattern compiles within 128 KiB, with a lazy-DFA cache of at most 128 KiB and nesting at most 32 deep. Matching is linear in the line's length.
- Lines are matched after mIRC colours and formatting, control characters and invisible format characters are removed. Tabs and the line and paragraph separators become spaces.
- The first pattern that matches wins. A captured field is trimmed of spaces. A line whose captures hold a URL (`://`) is refused, so a URL never reaches a stored claim.
- **`size`** reads a number with an optional unit: `B`; `KB`, `MB`, `GB`, `TB` (powers of 1,000); `KiB`, `MiB`, `GiB`, `TiB` (powers of 1,024), in any case. `1,024.5` groups thousands; `1,37` and `0,500` are decimal commas. A no-break space counts as a space. A size that doesn't read, or is over 1 PiB, is left out; the announce is kept.
- **`category`** is the tracker's own category text, cut to 256 bytes.
- **`id`** is the tracker's torrent id: letters, digits, `-`, `_` and `.`, at most 64 bytes, and not only dots.

### The URL template
- `https://`, a host name and an optional port (1 to 65535), then a path and query. No user name or password in it, and no placeholder in the host.
- Placeholders: `{id}`, `{name}` and `{passkey}` (at most once). Each value is percent-encoded. A template that uses `{id}` needs every pattern to capture `id`. No URL is made for an announce whose `{id}` or `{name}` value is only dots, since it would climb the path.

### Announcers
A line is an announce only when it's a message (not CTCP) to one of the definition's channels from one of its announcer nicks. Where the network lets anyone take a nick that isn't in use, add `announcer-host` lines: the message's host must then be one of them too (compared ignoring case).
