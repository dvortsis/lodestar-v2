// Settings → Outputs (OUT-2): what the screen derives from the server's answers. Nothing here
// holds a key's secret: B12 keeps only its hash, so a secret exists in the browser only in the
// reveal dialog's props, right after it's created or regenerated (docs/decisions/2026-11-12-OUT-2.md).
import type { components } from '$lib/api/schema';

export type Key = components['schemas']['KeyInfo'];
export type Job = components['schemas']['BackgroundJob'];

/** Where Torznab answers is `/api/v1/torznab/api` (TZ-0). Prowlarr, Sonarr and Radarr ask for a
 * URL and an "API Path" (default `/api`) and request the two joined, so the URL to paste stops
 * before the last `/api`. */
export const TORZNAB_BASE = '/api/v1/torznab';

/** The Torznab URL for this server as the browser reaches it: the address the apps need is the
 * same one unless they run elsewhere, which the help line says. */
export function torznabUrl(origin: string): string {
	return origin.replace(/\/+$/, '') + TORZNAB_BASE;
}

/** The apps a Torznab key is usually for (TZ-0's how-to): one key each, named for the app, so
 * each can be revoked alone. The name stays editable. */
export const APPS = ['Prowlarr', 'Sonarr', 'Radarr', 'Lidarr', 'Readarr'] as const;

/** Keys that answer Torznab: active or not, API keys with the `torznab` scope. Keys with other
 * scopes too are listed (they work) but marked, since the safe choice is `torznab` alone. */
export function torznabKeys(keys: readonly Key[]): Key[] {
	return keys.filter((k) => k.kind === 'api_key' && k.scopes.includes('torznab'));
}

/** Whether a key has scopes beyond `torznab` (TZ-0: works, but isn't the safe choice). */
export const wider = (k: Key): boolean => k.scopes.some((s) => s !== 'torznab');

/** The first app name no active Torznab key uses yet, for the new key's name; Prowlarr again if
 * every one is taken (the user edits it). */
export function suggestName(keys: readonly Key[]): string {
	const used = new Set(keys.filter((k) => k.status === 'active').map((k) => k.name.trim()));
	return APPS.find((a) => !used.has(a)) ?? 'Prowlarr';
}

/** A new key's name, checked as the server checks it (B12: 1–200 characters). */
export function nameError(name: string): 'required' | 'long' | null {
	const t = name.trim();
	if (!t) return 'required';
	if ([...t].length > 200) return 'long';
	return null;
}

/** The bitmagnet export: every torrent the export may include, in bitmagnet's import format
 * (OUT-0). The browser saves it to disk as it arrives; nothing holds it in memory. */
export const EXPORT_URL = '/api/v1/export?format=bitmagnet';

/** The highest export job id in a `/jobs` answer, 0 when there's none. Job ids only grow while the
 * server runs, so a job with a higher id started later. */
export function lastExportId(jobs: readonly Job[]): number {
	return jobs.reduce((top, j) => (j.kind === 'export' && j.id > top ? j.id : top), 0);
}

/** Seconds of slack around the click when no `/jobs` answer came before it (the fallback below
 * compares two clocks). */
const SLACK_S = 5;

/**
 * The export job this screen started: the newest export job with an id above `afterId`, the
 * highest export id seen before the click (`lastExportId`). Ids, not times: the browser's and the
 * server's clocks can differ. A download's own job id is only in its response header, which a
 * browser download doesn't expose, so the screen finds it this way.
 *
 * Only the newest is taken, never an older one, so a double press never takes the first press's
 * job for the second. While that newest job's format isn't known yet (`export` not set, just after
 * it starts) the answer is null: wait. Another format's export is someone else's and is passed over.
 *
 * With `afterId` null (no `/jobs` answer came before the click, or the server restarted) it falls
 * back to the job's start time against the click's, within `SLACK_S`.
 */
export function ourExport(
	jobs: readonly Job[],
	afterId: number | null,
	clickedAt: number
): Job | null {
	const newest = jobs.find(
		(j) =>
			j.kind === 'export' &&
			(j.export == null || j.export.format === 'bitmagnet') &&
			(afterId === null ? j.started_at >= clickedAt - SLACK_S : j.id > afterId)
	);
	return newest?.export ? newest : null;
}

export type ExportView =
	| { state: 'waiting' }
	| { state: 'running' | 'paused'; done: number; total: number | null; approx: boolean }
	| { state: 'done'; done: number; skipped: number; truncated: boolean; cap: number | null }
	| { state: 'cancelled' | 'failed'; done: number };

/** What the progress line shows, all of it the server's word. */
export function exportView(job: Job | null): ExportView {
	if (!job) return { state: 'waiting' };
	switch (job.state) {
		case 'running':
		case 'paused':
			return {
				state: job.state,
				done: job.done,
				total: job.total,
				approx: job.total_approx
			};
		case 'done':
			return {
				state: 'done',
				done: job.done,
				skipped: job.skipped,
				truncated: job.export?.truncated ?? false,
				cap: job.export?.cap ?? null
			};
		default:
			return { state: job.state, done: job.done };
	}
}

/** Whether to keep polling: until the job has finished, or for at most `giveUpS` without one. */
export function keepPolling(job: Job | null, waitedS: number, giveUpS = 30): boolean {
	return job ? job.finished_at === null : waitedS < giveUpS;
}
