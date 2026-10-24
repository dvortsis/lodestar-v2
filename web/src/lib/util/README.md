# lib/util

Small neutral helpers that are not metric formatters (those live in `format/`). Pure functions, no Svelte imports.

## Magnet and clipboard

- `magnet.ts`: `isMagnet(value)` is true only for a string starting `magnet:?`. The magnet link is the one URL the UI takes from torrent data; check it before putting it in an `href`.
- `clipboard.ts`: `copyText(text): Promise<'copied' | 'manual'>`. Uses `navigator.clipboard` in a secure context, else (or if that throws) a hidden-textarea `execCommand('copy')`. A plain-HTTP LAN address is not a secure context, so the fallback is the normal path on a typical home server. `'manual'` means both failed: show the text selected and tell the user to press Ctrl+C (⌘C on a Mac).

## Bidirectional controls

`bidi.ts`: `showBidi(text)` replaces U+202A–202E and U+2066–2069 with visible `<U+202E>` markers. Use it on any torrent name or path before it is shown, so `invoice‮fdp.exe` can't display as "invoiceexe.pdf".
