// "Removing X gives N": the counted clauses of a zero-result answer (loaded with Recover.svelte).
import type { components } from '$lib/api/schema';

type Recovery = components['schemas']['SearchRecovery'];

export interface Suggestion {
	clause: string;
	total: number;
	approx: boolean;
	without: components['schemas']['AstNode'];
}

/** "Removing X gives N": only clauses with a count above 0 (absent means the deadline passed). */
export function suggestions(recovery: Recovery | undefined | null): Suggestion[] {
	return (recovery?.clauses ?? []).flatMap((c) =>
		c.total !== undefined && c.total > 0
			? [{ clause: c.clause, total: c.total, approx: c.total_approx, without: c.without }]
			: []
	);
}
