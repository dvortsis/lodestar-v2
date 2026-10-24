// The alert page's keys (§10.16): j/k step through its alerts, in the `alerts` scope. Loaded after
// mount with the key registry, so the page's first view doesn't carry them (as Detail's commands).
import { keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';

export function registerAlertDetailCommands(step: (by: 1 | -1) => void): () => void {
	const pop = keys.pushScope('alerts');
	const offs = [
		keys.register({
			id: 'results.next',
			title: () => m.alerts_key_next(),
			section: () => m.alerts_section(),
			scope: 'alerts',
			palette: false,
			run: () => step(1)
		}),
		keys.register({
			id: 'results.previous',
			title: () => m.alerts_key_previous(),
			section: () => m.alerts_section(),
			scope: 'alerts',
			palette: false,
			run: () => step(-1)
		})
	];
	return () => {
		offs.forEach((off) => off());
		pop();
	};
}
