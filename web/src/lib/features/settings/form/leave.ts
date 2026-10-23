// The leave guard's decision (§10.10.5), pure so every case is tested without a router.
//   allow   — let the navigation happen
//   prompt  — cancel it and ask Save / Discard / Keep editing in the app
//   browser — cancel it so the browser asks (closing or reloading the tab)

export interface LeaveInput {
	/** Unsaved changes (dirty fields or conflicts). */
	dirty: boolean;
	/** SvelteKit's navigation type: 'leave' is closing or reloading the tab. */
	type: string;
	from: URL | null;
	to: URL | null;
	/** Set while the guard's own navigation after Save or Discard runs. */
	bypass: boolean;
}

export type LeaveDecision = 'allow' | 'prompt' | 'browser';

export function leaveDecision({ dirty, type, from, to, bypass }: LeaveInput): LeaveDecision {
	if (!dirty || bypass) return 'allow';
	if (type === 'leave') return 'browser';
	// A hash or query change on the same path isn't leaving the screen.
	if (from && to && from.origin === to.origin && from.pathname === to.pathname) return 'allow';
	return 'prompt';
}
