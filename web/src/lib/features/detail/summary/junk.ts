import { m } from '$lib/paraglide/messages.js';

// The reason keys are `crates/classify/src/junk.rs` `JunkReason::as_str`. An unknown key (a newer
// server) is shown as itself rather than hidden.
const TEXT: Record<string, () => string> = {
	'padding-only': () => m.detail_junk_padding_only(),
	empty: () => m.detail_junk_empty(),
	'double-extension': () => m.detail_junk_double_extension(),
	executable: () => m.detail_junk_executable(),
	'web-shortcut': () => m.detail_junk_web_shortcut(),
	'lure-file': () => m.detail_junk_lure_file(),
	'name-mismatch': () => m.detail_junk_name_mismatch(),
	'archive-for-media': () => m.detail_junk_archive_for_media(),
	'tiny-video': () => m.detail_junk_tiny_video()
};

export const junkText = (key: string): string => TEXT[key]?.() ?? key;
