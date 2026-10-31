// The Security screen's leave rule (§10.10.5), pure so a table test can hold it to the engine's.
// It's the engine's `leaveDecision` (form/leave.ts) restated: importing that would split leave.ts
// out of the engine's chunk and cost every Settings page. `leave.test.ts` checks the two agree
// over every case, so they can't drift apart.
import type { LeaveDecision, LeaveInput } from '../form/leave';

export function securityLeave({ dirty, type, from, to, bypass }: LeaveInput): LeaveDecision {
	if (!dirty || bypass) return 'allow';
	if (type === 'leave') return 'browser';
	if (from && to && from.origin === to.origin && from.pathname === to.pathname) return 'allow';
	return 'prompt';
}
