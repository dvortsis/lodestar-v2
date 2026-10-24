// Change builders: one per `op`, typed to `Change`, no logic beyond shaping. Whether a change
// weakens filtering, needs the password or is allowed at all is the server's answer to the
// preview, never the builder's.
import type { ChangeOf, MatchInput } from './types';

export const coreSwitch = (rule: string, on: boolean): ChangeOf<'core_switch'> => ({
	op: 'core_switch',
	rule,
	on
});

export const packSwitch = (id: string, on: boolean): ChangeOf<'pack_switch'> => ({
	op: 'pack_switch',
	id,
	on
});

export type AddRuleInput = Omit<ChangeOf<'add_rule'>, 'op'>;
export const addRule = (input: AddRuleInput): ChangeOf<'add_rule'> => ({
	...input,
	op: 'add_rule'
});

export type UpdateRuleInput = Omit<ChangeOf<'update_rule'>, 'op' | 'id'>;
export const updateRule = (id: number, patch: UpdateRuleInput): ChangeOf<'update_rule'> => ({
	...patch,
	op: 'update_rule',
	id
});

export const removeRule = (id: number): ChangeOf<'remove_rule'> => ({ op: 'remove_rule', id });

/** A CSX-5 entry: one match (`hashes`, `keyword`, `query` or `payload`) plus its options. */
export type OverrideInput = Omit<ChangeOf<'add_override'>, 'op'>;
export const addOverride = (input: OverrideInput): ChangeOf<'add_override'> => ({
	...input,
	op: 'add_override'
});

export const removeOverride = (id: number): ChangeOf<'remove_override'> => ({
	op: 'remove_override',
	id
});

export const addException = (match: MatchInput, rule: string): ChangeOf<'add_exception'> => ({
	...match,
	rule,
	op: 'add_exception'
});

export const removeException = (id: number): ChangeOf<'remove_exception'> => ({
	op: 'remove_exception',
	id
});

export const addAllow = (query: string): ChangeOf<'add_allow'> => ({ op: 'add_allow', query });
export const removeAllow = (id: number): ChangeOf<'remove_allow'> => ({ op: 'remove_allow', id });

export const release = (hash: string): ChangeOf<'release'> => ({ op: 'release', hash });
/** Confirm a review-queue torrent as abuse material (`op: confirm`). */
export const confirmAbuse = (hash: string): ChangeOf<'confirm'> => ({ op: 'confirm', hash });
export const falsePositive = (hash: string): ChangeOf<'false_positive'> => ({
	op: 'false_positive',
	hash
});
export const revealName = (hash: string): ChangeOf<'reveal_name'> => ({ op: 'reveal_name', hash });

export const reportAbuse = (hashes: string[]): ChangeOf<'report_abuse'> => ({
	op: 'report_abuse',
	hashes: [...hashes]
});
export const removeCs1 = (hashes: string[]): ChangeOf<'remove_cs1'> => ({
	op: 'remove_cs1',
	hashes: [...hashes]
});

/**
 * Settle a source's pending floor removals. With `remove: true` the flow sends back exactly the
 * `floor_entries` of the assessment on screen as `hashes` (never the builder's caller).
 */
export const floorRemoval = (
	source: string,
	remove: boolean
): ChangeOf<'resolve_floor_removal'> => ({
	op: 'resolve_floor_removal',
	source,
	remove
});

export const undoChange = (): ChangeOf<'undo'> => ({ op: 'undo' });

/** The ops that take `shadow_days` (§20.3, §20.5.5): "Shadow for N days" is offered only for these. */
export const SHADOW_OPS = ['add_rule', 'update_rule', 'add_override'] as const;
export type ShadowOp = (typeof SHADOW_OPS)[number];
export const canShadow = (change: { op: string }): change is ChangeOf<ShadowOp> =>
	(SHADOW_OPS as readonly string[]).includes(change.op);
