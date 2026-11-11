// The wizard's "Install qBittorrent for me" state (§10.10.10, §18.10, CMP-6's API). The server
// decides everything: whether it can be installed here (`refusal`), what it would fetch (the
// offer), and whether the confirmation matches. This file only carries the offer's own `version`
// and `sha256` back, unchanged, once the user has confirmed.
import { api, ApiError } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type Offer = components['schemas']['InstallOffer'];
export const COMPANION_ID = 'qbittorrent';

export type InstallPhase =
	| 'checking'
	/** Not offered here (Docker, Unraid, Windows, companions off) or already installed. */
	| 'hidden'
	| 'ready'
	| 'asking'
	| 'confirm'
	| 'installing'
	| 'done'
	| 'failed';

/** The server's own refusal codes, as the plain-words message each maps to. */
export const KNOWN_CODES = [
	'native_in_container',
	'unsupported_platform',
	'awaiting_pin',
	'no_build',
	'guided_only',
	'already_current',
	'confirmation_mismatch',
	'not_wired',
	'busy',
	'companions_unavailable'
] as const;
export type KnownCode = (typeof KNOWN_CODES)[number];
export type Failure = KnownCode | 'other';

const known = (code: string): Failure =>
	(KNOWN_CODES as readonly string[]).includes(code) ? (code as KnownCode) : 'other';

export function createInstall(onInstalled: () => void) {
	let phase = $state<InstallPhase>('checking');
	let offer = $state<Offer | null>(null);
	let failure = $state<Failure | null>(null);
	let gone = false;
	let run = 0;

	/** Reads whether this build can install it. Only an explicit "yes" from the server shows it. */
	async function check() {
		try {
			const { companions } = await api.get('/companions');
			if (gone) return;
			const row = companions.find((c) => c.id === COMPANION_ID);
			phase =
				row && row.refusal === null && row.available !== null && row.installed === null
					? 'ready'
					: 'hidden';
		} catch {
			if (!gone) phase = 'hidden';
		}
	}

	/**
	 * The install reply can be lost (a proxy timeout, a dropped connection) while the server
	 * finishes. So after any failure the server is asked again, and an installed client counts as done.
	 */
	async function fail(e: unknown) {
		const code = e instanceof ApiError ? known(e.code) : 'other';
		failure = code;
		offer = null;
		phase = 'failed';
		if (code === 'confirmation_mismatch' || code === 'native_in_container') return;
		try {
			const { companions } = await api.get('/companions');
			if (gone || phase !== 'failed') return;
			if (companions.find((c) => c.id === COMPANION_ID)?.installed != null) finish();
		} catch {
			// Keep the failure on screen.
		}
	}

	function finish() {
		phase = 'done';
		offer = null;
		failure = null;
		onInstalled();
	}

	/** Step 1: fetch what would be installed. Nothing is installed. */
	async function ask() {
		if (phase !== 'ready' && phase !== 'failed') return;
		const id = ++run;
		phase = 'asking';
		failure = null;
		try {
			const next = await api.get('/companions/{id}/install', { path: { id: COMPANION_ID } });
			if (gone || id !== run) return;
			offer = next;
			phase = 'confirm';
		} catch (e) {
			if (!gone && id === run) await fail(e);
		}
	}

	/** Step 2: the user confirmed the offer on screen. Sends that offer's own version and hash. */
	async function confirm() {
		if (phase !== 'confirm' || !offer) return;
		const { version, sha256 } = offer;
		const id = ++run;
		phase = 'installing';
		failure = null;
		try {
			await api.post('/companions/{id}/install', {
				path: { id: COMPANION_ID },
				body: { version, sha256 }
			});
			if (gone || id !== run) return;
			finish();
		} catch (e) {
			if (!gone && id === run) await fail(e);
		}
	}

	/** Back out before anything was sent. There is no cancel once the install has started. */
	function decline() {
		if (phase !== 'confirm') return;
		run++;
		offer = null;
		phase = 'ready';
	}

	return {
		get phase() {
			return phase;
		},
		get offer() {
			return offer;
		},
		get failure() {
			return failure;
		},
		check,
		ask,
		confirm,
		decline,
		destroy() {
			gone = true;
		}
	};
}
