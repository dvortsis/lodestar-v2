// The Privacy dialog for turning on query logging (§10.10.7), loaded when the gate first runs.
import { formatCount } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';
import { ask } from '../form/dialogs';

/** Says what is recorded; true when the user confirms. Focus starts on Cancel. */
export async function confirmQueryLogging(retentionDays: number): Promise<boolean> {
	const days = formatCount(retentionDays || 30);
	return (
		(await ask({
			title: m.settings_screen_privacy_title(),
			body: [
				m.settings_screen_privacy_what(),
				m.settings_screen_privacy_where(),
				m.settings_screen_privacy_days({ days })
			],
			actions: [
				{ id: 'cancel', label: m.settings_form_cancel() },
				{ id: 'on', label: m.settings_screen_privacy_confirm() }
			],
			initial: 'cancel'
		})) === 'on'
	);
}
