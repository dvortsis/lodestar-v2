// The Alerts pages' keys (§10.16), in the `alerts` scope: the list's New alert command, and j/k
// stepping through an alert's matches. Loaded after mount with the key registry, so neither page's
// first view carries them (as Detail's commands).
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

export function registerAlertsListCommands(create: () => void): () => void {
	const pop = keys.pushScope('alerts');
	const off = keys.register({
		id: 'alerts.new',
		title: () => m.alerts_new_command(),
		section: () => m.alerts_section(),
		scope: 'alerts',
		run: create
	});
	return () => {
		off();
		pop();
	};
}
