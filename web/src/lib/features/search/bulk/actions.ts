// The bulk bar's actions (§10.4): Copy magnets and Export both ask `POST /export` for the selected
// hashes (the server builds the magnets), Compare links to `/compare`. Each returns a result the
// bar reports; none throws.
import { copyText } from '$lib/util/clipboard';
import { isMagnet } from '$lib/util/magnet';

// lib/api owns the base path but doesn't export it, and the client only reads JSON.
const BASE = '/api/v1';

/** §10.3: Compare takes two to five torrents. */
export const COMPARE_MIN = 2;
export const COMPARE_MAX = 5;
/** `POST /export` takes at most this many hashes. */
export const EXPORT_MAX = 10_000;

export const canCompare = (n: number) => n >= COMPARE_MIN && n <= COMPARE_MAX;

export type ExportResult =
	{ ok: true; text: string; filename: string } | { ok: false; reason: 'too_many' | 'failed' };

/** Fetch an export of `hashes` as text. */
export async function exportHashes(
	hashes: readonly string[],
	format: 'magnets' | 'csv',
	fetcher: typeof fetch = fetch
): Promise<ExportResult> {
	if (hashes.length > EXPORT_MAX) return { ok: false, reason: 'too_many' };
	try {
		const res = await fetcher(`${BASE}/export`, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', Accept: 'text/plain, text/csv' },
			body: JSON.stringify({ hashes, format }),
			credentials: 'same-origin'
		});
		if (!res.ok) return { ok: false, reason: 'failed' };
		const disposition = res.headers.get('Content-Disposition') ?? '';
		const named = /filename="?([^";]+)"?/.exec(disposition)?.[1];
		return {
			ok: true,
			text: await res.text(),
			filename: named ?? `lodestar-export.${format === 'csv' ? 'csv' : 'txt'}`
		};
	} catch {
		return { ok: false, reason: 'failed' };
	}
}

/** The magnets, one per line. The count is the lines the server wrote. */
export async function copyMagnets(hashes: readonly string[], fetcher?: typeof fetch) {
	const r = await exportHashes(hashes, 'magnets', fetcher);
	if (!r.ok) return r;
	const lines = r.text.split('\n').filter((l) => isMagnet(l));
	if (lines.length === 0) return { ok: false as const, reason: 'failed' as const };
	const copied = await copyText(lines.join('\n'));
	return {
		ok: true as const,
		count: lines.length,
		copied: copied === 'copied',
		text: lines.join('\n')
	};
}

/** Save `text` as a file download. */
export function download(text: string, filename: string, type: string) {
	const url = URL.createObjectURL(new Blob([text], { type }));
	const a = document.createElement('a');
	a.href = url;
	a.download = filename;
	document.body.appendChild(a);
	a.click();
	a.remove();
	setTimeout(() => URL.revokeObjectURL(url), 0);
}
