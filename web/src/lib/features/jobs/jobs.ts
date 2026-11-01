// What the Jobs screen derives from `/jobs`. Everything here is the server's word: a state is
// shown only once the server reports it.
import type { components } from '$lib/api/schema';

export type Job = components['schemas']['BackgroundJob'];
export type JobView = 'running' | 'paused' | 'cancelling' | 'cancelled' | 'done' | 'failed';
export type JobControl = 'pause' | 'resume' | 'cancel';

/** `cancelled` without a finish time is a cancel the worker hasn't reached yet. */
export const viewOf = (j: Job): JobView =>
	j.state === 'cancelled' && j.finished_at === null ? 'cancelling' : j.state;

export const isActive = (j: Job): boolean => j.finished_at === null;

/** Items read so far: the server counts skipped ones in `total`. */
export const processed = (j: Job): number => j.done + j.skipped;

/** 0–1, or null while the total is unknown. */
export function ratio(j: Job): number | null {
	if (j.total === null) return null;
	if (j.total === 0) return j.finished_at === null ? 0 : 1;
	return Math.min(1, processed(j) / j.total);
}

/** The controls the server will accept for this job right now. */
export function controls(j: Job): JobControl[] {
	switch (viewOf(j)) {
		case 'running':
			return ['pause', 'cancel'];
		case 'paused':
			return ['resume', 'cancel'];
		default:
			return [];
	}
}

/** Whether to poll quickly: something is still moving. */
export const anyActive = (jobs: readonly Job[]): boolean => jobs.some(isActive);
