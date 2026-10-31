// Start-up for the region settings: the cached hint is applied before the first paint, then the
// account's saved values replace it. A failed, slow or malformed answer keeps the hint and never
// blocks or throws (§10.12), and an answer older than a value applied meanwhile is dropped. Not
// awaited by the layout. A plain `fetch` keeps the API client out of the first load (§10.1).
import {
	applyRegion,
	narrowRegion,
	readRegionHint,
	regionSeq,
	writeRegionHint
} from './region.svelte';

const WAKE = ['pointerdown', 'keydown'] as const;

function fetchSaved(): Promise<void> {
	const started = regionSeq();
	return fetch('/api/v1/settings/preferences', { headers: { accept: 'application/json' } })
		.then((r) => {
			// Signed out (login page): try again on the next interaction, once signed in.
			if (r.status === 401) rearm();
			return r.ok ? r.json() : null;
		})
		.then((res: { fields?: Record<string, { value?: unknown }> } | null) => {
			const f = res?.fields;
			if (!f || regionSeq() !== started) return;
			const saved = narrowRegion({
				language: f.language?.value,
				time_zone: f.time_zone?.value,
				first_day_of_week: f.first_day_of_week?.value
			});
			applyRegion(saved);
			writeRegionHint(saved);
		})
		.catch(() => {
			// Offline or a server error keeps the hint.
		});
}

function rearm(): void {
	const go = () => {
		for (const e of WAKE) removeEventListener(e, go);
		void fetchSaved();
	};
	for (const e of WAKE) addEventListener(e, go, { once: true });
}

export function bootRegion(): Promise<void> {
	applyRegion(readRegionHint());
	return fetchSaved();
}
