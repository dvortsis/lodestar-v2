// "Block this…" (§20.2): suggestions narrowest first, and the phrase picker's query. These only
// shape a query; the rule it becomes still goes through the preview and the server's answer.

export type BlockTier = 'torrent' | 'payload' | 'group' | 'phrase';
/** Why a tier can't be offered yet: a stable code the dialog words. */
export type BlockUnavailable = 'no_payload_signature' | 'no_group';

export interface BlockSuggestion {
	tier: BlockTier;
	available: boolean;
	reason?: BlockUnavailable;
	/** The rule's query, for the tiers that have a fixed one. */
	query?: string;
}

export interface BlockTarget {
	hash: string;
	hash_v2: string | null;
	name: string;
}

const HEX40 = /^[0-9a-f]{40}$/;
const HEX64 = /^[0-9a-f]{64}$/;

/**
 * `hash:<v1>`, or `hash:<v1> | hash:<v2>` for a torrent with a v2 hash. The query language's
 * `hash:` takes 40 hex digits, and the index keeps a v2 hash truncated to its first 20 bytes (as
 * the DHT does), so the v2 form is the first 40 digits of `hash_v2`.
 */
export function torrentQuery(t: Pick<BlockTarget, 'hash' | 'hash_v2'>): string | null {
	const v1 = t.hash.toLowerCase();
	if (!HEX40.test(v1)) return null;
	const v2 = t.hash_v2?.toLowerCase();
	const short = v2 && HEX64.test(v2) ? v2.slice(0, 40) : null;
	return short && short !== v1 ? `hash:${v1} | hash:${short}` : `hash:${v1}`;
}

/** Splits a torrent's name into words: whitespace and `. _ - [ ] ( )` separate them. */
export function nameWords(name: string): string[] {
	return name.split(/[\s._\-[\]()]+/u).filter(Boolean);
}

/** A phrase is at most this many UTF-8 bytes. */
export const MAX_PHRASE_BYTES = 200;

/**
 * `name:"<words>"` from the words picked, or null when nothing usable is left. Every `"` is removed
 * (the language has no escape inside a phrase) and whitespace collapses to single spaces.
 */
export function phraseQuery(words: string[]): string | null {
	const phrase = words.join(' ').replaceAll('"', '').replace(/\s+/gu, ' ').trim();
	if (!phrase || new TextEncoder().encode(phrase).length > MAX_PHRASE_BYTES) return null;
	return `name:"${phrase}"`;
}

/**
 * The tiers, narrowest first: this torrent, its payload signature, its uploader or group, then a
 * phrase from its name (never preselected). Search results and the detail drawer carry no payload
 * signature (`sig:` arrives with M7) and no release group or uploader yet, so tiers 2 and 3 are
 * listed as unavailable.
 */
export function suggestBlocks(t: BlockTarget): BlockSuggestion[] {
	const torrent = torrentQuery(t);
	return [
		torrent
			? { tier: 'torrent', available: true, query: torrent }
			: { tier: 'torrent', available: false },
		{ tier: 'payload', available: false, reason: 'no_payload_signature' },
		{ tier: 'group', available: false, reason: 'no_group' },
		{ tier: 'phrase', available: nameWords(t.name).length > 0 }
	];
}
