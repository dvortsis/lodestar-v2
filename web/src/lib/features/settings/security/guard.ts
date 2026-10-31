// One leave guard for the whole Security screen. Each part with typed input (Change password, a
// new key, the proxy settings) calls `guardLeave` while it initialises; the first one still on
// screen decides for all of them, so leaving asks once and Discard clears every part that has
// something typed. Nothing typed here reaches the engine's sessionStorage draft (§10.10.5).
import { onDestroy } from 'svelte';
import { beforeNavigate, goto } from '$app/navigation';
import { m } from '$lib/paraglide/messages.js';
import { ask } from '../form/dialogs';
import { securityLeave } from './leave';

export interface Guarded {
	dirty: () => boolean;
	clear: () => void;
	/** The part's heading, named in the question. */
	section: () => string;
}

const parts: Guarded[] = [];
let bypass = false;

export function guardLeave(part: Guarded): void {
	parts.push(part);
	onDestroy(() => {
		const i = parts.indexOf(part);
		if (i >= 0) parts.splice(i, 1);
	});
	beforeNavigate((nav) => {
		if (parts[0] !== part) return;
		const typed = parts.filter((p) => p.dirty());
		const to = nav.to?.url ?? null;
		const decision = securityLeave({
			dirty: typed.length > 0,
			type: nav.type,
			from: nav.from?.url ?? null,
			to,
			bypass
		});
		if (decision === 'allow') return;
		nav.cancel();
		if (decision === 'browser' || !to) return;
		void ask({
			title: m.settings_security_leave_title(),
			body: typed.map((p) => m.settings_security_leave_body({ section: p.section() })),
			actions: [
				{ id: 'stay', label: m.settings_security_leave_stay() },
				{ id: 'leave', label: m.settings_security_leave_discard(), variant: 'danger' }
			],
			initial: 'stay'
		})
			// The question's code couldn't load: leave as asked, and the page takes what was typed.
			.catch(() => {
				location.assign(to.href);
				return null;
			})
			.then(async (answer) => {
				if (answer !== 'leave') return;
				for (const p of typed) p.clear();
				if (to.origin !== location.origin) return location.assign(to.href);
				bypass = true;
				try {
					// eslint-disable-next-line svelte/no-navigation-without-resolve -- the router's own target, already resolved
					await goto(to.href);
				} finally {
					bypass = false;
				}
			});
	});
}
