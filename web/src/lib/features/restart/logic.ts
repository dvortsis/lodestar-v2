// What the restart banner offers (§10.10.6), as pure decisions. The flow's own logic is in
// `flow.ts`.
import type { components } from '$lib/api/schema';

export type System = components['schemas']['SystemInfo'];

export interface Pending {
	/** Saved settings that wait for a restart, as `section.field`. */
	fields: string[];
	/** Whether the server can restart itself: the button shows only then. */
	canRestart: boolean;
	/** How to restart instead, when it can't. */
	hint: string | null;
	/** About how long a restart pauses crawling, from the server. */
	estimateSeconds: number;
	/** Where the UI will be after the restart, when the HTTP address changed (see `usableUrl`). */
	newUrl: string | null;
}

/** What the full-page Restarting state waits for. */
export interface Waiting {
	/** Where the UI will be when the HTTP address changed; null to stay on this address. */
	target: string | null;
	/** The run that is ending (`GET /system` `started_at`); null when unknown. */
	startedAt: number | null;
	estimateSeconds: number;
}

/** What the banner shows: nothing, or the pending changes. */
export function pendingOf(system: System | undefined): Pending | null {
	// A partial `/system` (a test's fixture, an older server) has nothing pending.
	if (!system?.restart_required?.length) return null;
	const change = system.address_change;
	return {
		fields: system.restart_required,
		canRestart: system.restart?.available ?? false,
		hint: system.restart?.hint ?? null,
		estimateSeconds: system.restart?.estimated_seconds ?? 0,
		newUrl:
			system.restart_required.includes('network.http_bind') && change?.state === 'awaiting_restart'
				? change.url
				: null
	};
}
