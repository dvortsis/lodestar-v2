// Turning rule packs on through the filters change flows (§20.6, §20.10): each pack is its own
// `pack_switch` change, previewed first like any other. The wizard only ever turns packs ON, and
// only applies what the server says needs no confirmation; anything else (a warning, a password)
// stops here and is left to Settings → Filters & Safety. The flows are passed in, loaded on demand
// by the pack step, so none of this is in the wizard's first view.
import type { FlowState } from '../filters/flows/machine';
import type { ChangeFlow } from '../filters/flows/flow.svelte';
import type { Change } from '../filters/flows/types';

const packOn = (id: string): Change => ({ op: 'pack_switch', id, on: true });
/** What a refused change says: the server's sentence, or nothing, so the caller words it. */
const said = (e: { message: string; code: string }) => e.message.trim() || e.code;

// Only what the Filters screen itself loads, loaded the same way (`flows/flow.svelte`, a dynamic
// import of a module that screen already imports dynamically), and the change as a typed literal:
// importing the flows' index or `errors.ts` from here would make a second consumer of the dialogs'
// code and split its chunks (docs/decisions/2026-10-31-S-wizard.md, part 2).
export type Flows = typeof import('../filters/flows/flow.svelte');

export interface PackFailure {
	id: string;
	/** The server's message, or null when the change needs a confirmation the wizard doesn't take. */
	message: string | null;
}

export interface PackResult {
	/** Packs the server now has on because of this run. */
	applied: string[];
	failed?: PackFailure;
}

/** Resolves with the first state of `flow` whose kind is in `kinds` (or `closed` on abort). */
function settle(
	flow: ChangeFlow & { readonly state: FlowState },
	kinds: string[],
	signal: AbortSignal
) {
	return new Promise<FlowState>((resolve) => {
		const root: { stop?: () => void } = {};
		let finished = false;
		const finish = (state: FlowState) => {
			if (finished) return;
			finished = true;
			queueMicrotask(() => root.stop?.());
			resolve(state);
		};
		root.stop = $effect.root(() => {
			$effect(() => {
				const state = flow.state;
				if (kinds.includes(state.kind)) finish(state);
			});
		});
		if (signal.aborted) finish({ kind: 'closed' });
		else signal.addEventListener('abort', () => finish({ kind: 'closed' }), { once: true });
	});
}

/** Turns the packs on, one at a time, in order; stops at the first that doesn't go through. */
export async function applyPacks(
	ids: string[],
	flows: Flows,
	signal: AbortSignal
): Promise<PackResult> {
	const applied: string[] = [];
	for (const id of ids) {
		const flow = flows.createChangeFlow();
		const stopOnAbort = () => flow.cancel();
		signal.addEventListener('abort', stopOnAbort, { once: true });
		try {
			flow.start(packOn(id));
			const preview = await settle(flow, ['review', 'confirm', 'failed', 'closed'], signal);
			if (preview.kind === 'failed')
				return { applied, failed: { id, message: said(preview.error) } };
			// Only a plain strengthening change goes through here. A warning or a password means the
			// server sees it as weakening something: the wizard doesn't take that decision.
			if (preview.kind !== 'review' || preview.assessment.weakening || preview.assessment.confirm) {
				flow.cancel();
				return { applied, failed: { id, message: null } };
			}
			flow.apply();
			const result = await settle(flow, ['done', 'failed', 'confirm', 'closed'], signal);
			if (result.kind === 'done') {
				applied.push(id);
				continue;
			}
			const message = result.kind === 'failed' ? said(result.error) : null;
			flow.cancel();
			return { applied, failed: { id, message } };
		} finally {
			signal.removeEventListener('abort', stopOnAbort);
		}
	}
	return { applied };
}
