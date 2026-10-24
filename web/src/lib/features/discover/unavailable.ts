// How an unavailable section reads. The switch is exhaustive on the generated `Unavailable`
// union, so a reason the server adds fails type-checking here until it has words.
import { m } from '$lib/paraglide/messages.js';
import type { components } from '$lib/api/schema';

export type Unavailable = components['schemas']['Unavailable'];

export interface UnavailableText {
	text: string;
	/** The one next step, only when there is one. */
	action?: { label: string; href: string };
}

export function unavailableText(reason: Unavailable): UnavailableText {
	switch (reason) {
		case 'search_disabled':
			return {
				text: m.discover_unavailable_search_disabled(),
				action: {
					label: m.discover_unavailable_search_disabled_action(),
					href: '/settings/search'
				}
			};
		case 'no_title':
			return { text: m.discover_unavailable_no_title() };
		case 'needs_swarm_history':
			return { text: m.discover_unavailable_needs_swarm_history() };
		case 'needs_resolution_history':
			return { text: m.discover_unavailable_needs_resolution_history() };
		case 'needs_payload_signature':
			return { text: m.discover_unavailable_needs_payload_signature() };
		case 'needs_piece_roots':
			return { text: m.discover_unavailable_needs_piece_roots() };
		default: {
			const unreachable: never = reason;
			return unreachable;
		}
	}
}
