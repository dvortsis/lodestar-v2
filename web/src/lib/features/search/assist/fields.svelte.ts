// The Builder's field list, from `GET /search/fields` (§8, §10.4): every field the parser accepts,
// with its type, operators and fixed values. The server leaves out the 18+ names while 18+ content
// is hidden (§20.9), so this file holds no field names: with no answer there is no list, never a
// built-in one. It is fetched each time the Builder opens (two static tables on the server, no index
// read) and dropped while the answer is out, so a list from before the setting changed is never
// shown. It doesn't listen to the event stream: that would load its module with the Builder.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type Field = components['schemas']['SearchField'];
export type Op = ':' | '>=' | '<=' | '>' | '<';

const OPS: Partial<Record<Field['operators'][number], Op>> = {
	eq: ':',
	gte: '>=',
	lte: '<=',
	gt: '>',
	lt: '<'
};

/** The operators a row can offer for a field: the server's, in the server's order. */
export function opsOf(field: Field | undefined): Op[] {
	if (field?.type === 'share') return [':']; // `cat:video>=50%`: the comparison is in the value
	const ops = field?.operators.map((o) => OPS[o]).filter((o): o is Op => o !== undefined) ?? [];
	return ops.length ? ops : [':'];
}

export function createFields() {
	let status = $state<'loading' | 'ready' | 'error'>('loading');
	let list = $state.raw<Field[]>([]);
	let run = 0;

	async function fetchFields() {
		const mine = ++run;
		status = 'loading';
		list = []; // nothing shows while the answer for the current setting is out
		try {
			const r = await api.get('/search/fields', {});
			if (mine !== run) return;
			list = r.fields;
			status = 'ready';
		} catch {
			if (mine !== run) return;
			status = 'error';
		}
	}

	return {
		get status() {
			return status;
		},
		get fields() {
			return list;
		},
		find: (name: string) => list.find((f) => f.name === name),
		/** Asks again each time the Builder opens: the list can't outlive a change of the setting. */
		ensure: fetchFields,
		/** Retry after a failure, or refetch. */
		refresh: fetchFields
	};
}

export const searchFields = createFields();
