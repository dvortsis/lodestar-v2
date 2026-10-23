// Admin password rules (architecture §12, §16 M2): at least 10 characters (Unicode scalar values,
// as the server counts them) and at most 1,024 bytes of UTF-8. Anything in between is accepted;
// the strength hint only advises, so a long passphrase is never blocked.

export const MIN_PASSWORD_CHARS = 10;
export const MAX_PASSWORD_BYTES = 1024;
/** From this length a password counts as a passphrase and is strong whatever it's made of. */
const PASSPHRASE_CHARS = 16;

export type PasswordCheck =
	/** Blocking: fewer than 10 characters. */
	| 'too_short'
	/** Blocking: over 1,024 bytes. */
	| 'too_long'
	/** Accepted, but repeats or a run of keys: easy to guess. */
	| 'weak'
	/** Accepted; longer would be better. */
	| 'fair'
	| 'strong';

export const isBlocking = (c: PasswordCheck) => c === 'too_short' || c === 'too_long';

export const passwordChars = (pw: string) => [...pw].length;

const utf8Bytes = (s: string) => new TextEncoder().encode(s).length;

/** Rows people type as "random": digits, the alphabet and keyboard rows, both ways. */
const RUNS = ['01234567890', 'abcdefghijklmnopqrstuvwxyz', 'qwertyuiop', 'asdfghjkl', 'zxcvbnm'];
const runs = [...RUNS, ...RUNS.map((r) => [...r].reverse().join(''))];
/** Words that top every leaked-password list, and this app's own name. */
const COMMON = ['password', 'passwort', 'qwerty', 'letmein', 'welcome', 'admin', 'lodestar'];

/** All of it is one run, or a common word with only digits and symbols around it. */
function isPredictable(pw: string): boolean {
	const lower = pw.toLowerCase();
	if (runs.some((r) => r.includes(lower))) return true;
	const letters = lower.replace(/[^\p{L}]/gu, '');
	return COMMON.includes(letters);
}

export function checkPassword(pw: string): PasswordCheck {
	const chars = [...pw];
	if (chars.length < MIN_PASSWORD_CHARS) return 'too_short';
	if (utf8Bytes(pw) > MAX_PASSWORD_BYTES) return 'too_long';
	const unique = new Set(chars.map((c) => c.toLowerCase())).size;
	if (unique <= 3 || isPredictable(pw)) return 'weak';
	if (chars.length >= PASSPHRASE_CHARS) return 'strong';
	const kinds = [/\p{Ll}/u, /\p{Lu}/u, /\p{N}/u, /[^\p{Ll}\p{Lu}\p{N}]/u].filter((r) =>
		r.test(pw)
	).length;
	return chars.length >= 12 && kinds >= 3 ? 'strong' : 'fair';
}
