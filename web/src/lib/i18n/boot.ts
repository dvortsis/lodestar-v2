// Start-up for the region settings: the cached hint is applied before the first paint, then the
// account's saved values replace it. A failed, slow or malformed answer keeps the hint and never
// blocks or throws (§10.12). Not awaited by the layout. A plain `fetch` keeps the API client out
// of the first load (§10.1); a 401 is the shell's to handle on its own requests.
import { applyRegion, followSaved, narrowRegion, readRegionHint } from './region.svelte';

export function bootRegion(): Promise<void> {
	applyRegion(readRegionHint());
	return fetch('/api/v1/settings/preferences', { headers: { accept: 'application/json' } })
		.then((r) => (r.ok ? r.json() : null))
		.then((res: { fields?: Record<string, { value?: unknown }> } | null) => {
			const f = res?.fields;
			if (!f) return;
			followSaved(
				narrowRegion({
					language: f.language?.value,
					time_zone: f.time_zone?.value,
					first_day_of_week: f.first_day_of_week?.value
				})
			);
		})
		.catch(() => {
			// Offline or a server error keeps the hint.
		});
}
