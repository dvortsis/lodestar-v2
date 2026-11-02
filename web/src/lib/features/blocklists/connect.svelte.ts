// Connecting catalog lists from the wizard (§20.13), headless: each list is its own `subscribe`
// change, previewed first like any other. The wizard only connects what the server says needs no
// confirmation (no `floor`, so no password); anything else stops here and is left to Settings. The
// flows are passed in, loaded on demand by the step, so none of this is in the wizard's first view.
import type { ChangeFlow } from '../filters/flows/flow.svelte';
import type { FlowState } from '../filters/flows/machine';
import { connect, refusalText, type CatalogList } from './lists';

export type Flows = Pick<typeof import('../filters/flows/flow.svelte'), 'createChangeFlow'>;

export interface ConnectFailure {
	list: CatalogList;
	/** The server's message, or null when the change needs a confirmation the wizard doesn't take. */
	message: string | null;
}
export interface ConnectResult {
	applied: string[];
	failed?: ConnectFailure;
}

/** Resolves with the first state of `flow` whose kind is in `kinds` (`closed` on abort). */
function settle(flow: ChangeFlow, kinds: string[], signal: AbortSignal) {
	return new Promise<FlowState>((resolve) => {
		const root: { stop?: () => void } = {};
		let finished = false;
		const onAbort = () => finish({ kind: 'closed' });
		const finish = (state: FlowState) => {
			if (finished) return;
			finished = true;
			signal.removeEventListener('abort', onAbort);
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
		else signal.addEventListener('abort', onAbort, { once: true });
	});
}

const plain = (s: FlowState) =>
	s.kind === 'review' && !s.assessment.weakening && !s.assessment.confirm && !s.assessment.warning;

/** Connects the lists in order; stops at the first that doesn't go through. */
export async function connectLists(
	lists: CatalogList[],
	flows: Flows,
	signal: AbortSignal
): Promise<ConnectResult> {
	const applied: string[] = [];
	for (const list of lists) {
		if (signal.aborted) return { applied };
		const flow = flows.createChangeFlow();
		const stopOnAbort = () => flow.cancel();
		signal.addEventListener('abort', stopOnAbort, { once: true });
		try {
			flow.start(connect(list));
			let state = await settle(flow, ['review', 'confirm', 'failed', 'closed'], signal);
			let message: string | null = null;
			if (state.kind === 'failed')
				return { applied, failed: { list, message: refusalText(state.error) } };
			// A change that moved since its preview comes back as a new preview: one more apply.
			for (let attempt = 0; attempt < 2; attempt++) {
				if (!plain(state)) {
					flow.cancel();
					return { applied, failed: { list, message } };
				}
				flow.apply();
				state = await settle(flow, ['done', 'failed', 'confirm', 'review', 'closed'], signal);
				if (state.kind === 'done') break;
				if (state.kind === 'failed') {
					message = refusalText(state.error);
					break;
				}
			}
			if (state.kind === 'done') {
				applied.push(list.key);
				continue;
			}
			flow.cancel();
			return { applied, failed: { list, message } };
		} finally {
			signal.removeEventListener('abort', stopOnAbort);
		}
	}
	return { applied };
}
