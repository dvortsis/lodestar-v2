// Copy and open the magnet link (§10.16 `c` and `o`). Magnets come from the detail body, the only
// place the server sends one; the scheme is checked before anything uses it as a URL.
import { abortError, api } from '$lib/api/client';
import { copyText } from '$lib/util/clipboard';
import { isMagnet } from '$lib/util/magnet';
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { m } from '$lib/paraglide/messages.js';
import { cacheDetail, detailGeneration, forgetDetail, onDetailInvalidated } from './cache';
import { manual } from './manual.svelte';

/** Show the manual field, mounting it once on demand (the `c` key can run with no drawer open). */
async function showManual(magnet: string, generation: number) {
	const mod = await import('./manual-mount');
	if (generation !== detailGeneration()) return;
	manual.magnet = magnet;
	mod.mountManual();
}

export async function magnetFor(hash: string): Promise<string | null> {
	const generation = detailGeneration();
	const ctl = new AbortController();
	const off = onDetailInvalidated(() => ctl.abort());
	try {
		const detail = await api.get('/torrents/{hash}', { path: { hash }, signal: ctl.signal });
		if (ctl.signal.aborted || generation !== detailGeneration()) throw abortError();
		cacheDetail(hash, detail);
		return isMagnet(detail.magnet) ? detail.magnet : null;
	} catch (e) {
		if (generation === detailGeneration()) forgetDetail(hash);
		throw e;
	} finally {
		off();
	}
}

/** Copy `magnet` (or the one for `hash`). Toasts on success; on failure opens the manual field. */
export async function copyMagnet(source: { magnet: string } | { hash: string }) {
	const generation = detailGeneration();
	let magnet: string | null = null;
	try {
		magnet =
			'magnet' in source
				? isMagnet(source.magnet)
					? source.magnet
					: null
				: await magnetFor(source.hash);
	} catch {
		magnet = null;
	}
	if (generation !== detailGeneration()) return 'failed' as const;
	if (!magnet) {
		toast.push({ kind: 'error', text: m.detail_magnet_unavailable() });
		return 'failed' as const;
	}
	const result = await copyText(magnet);
	if (generation !== detailGeneration()) return 'failed' as const;
	if (result === 'copied') toast.push({ kind: 'success', text: m.detail_magnet_copied() });
	else await showManual(magnet, generation);
	return result;
}

/** Hand the magnet to the system's torrent client by following it as a link. */
export async function openMagnet(source: { magnet: string } | { hash: string }) {
	const generation = detailGeneration();
	let magnet: string | null = null;
	try {
		magnet = 'magnet' in source ? source.magnet : await magnetFor(source.hash);
	} catch {
		magnet = null;
	}
	if (generation !== detailGeneration()) return false;
	if (!isMagnet(magnet)) {
		toast.push({ kind: 'error', text: m.detail_magnet_unavailable() });
		return false;
	}
	const a = document.createElement('a');
	a.href = magnet;
	a.rel = 'noopener';
	document.body.appendChild(a);
	a.click();
	a.remove();
	return true;
}
