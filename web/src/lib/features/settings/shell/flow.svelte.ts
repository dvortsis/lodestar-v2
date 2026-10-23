// Where a restart or an address change has got to (§10.10.6). The layout watches `phase` to bring
// in the full-page "Restarting…" state (lazy); `restart-flow.ts` starts it.
export interface Flow {
	/** `idle` is nothing; `waiting` polls for the server; `timeout` gave up after two minutes. */
	phase: 'idle' | 'waiting' | 'timeout';
	/** Another address to move to when it answers (an address change); undefined for this one. */
	to?: string;
	/** Seconds the restart is expected to take, for the words. */
	estimate: number;
	/** `started_at` of the process that was running when the restart began. */
	before: number;
}

export const flow = $state<Flow>({ phase: 'idle', estimate: 0, before: 0 });

export function startWaiting(next: { to?: string; estimate: number; before: number }): void {
	flow.to = next.to;
	flow.estimate = next.estimate;
	flow.before = next.before;
	flow.phase = 'waiting';
}

export function stopWaiting(): void {
	flow.phase = 'idle';
	flow.to = undefined;
}
