// The outputs' Privacy question (§10.10.7, §12.1), loaded with the first save that needs it: the
// console's detail set to "same as the file" sends any torrent details the file gets to the
// console too, and so to `docker logs` or the journal, which Lodestar can't delete.
import { m } from '$lib/paraglide/messages.js';
import { ask } from '../form/dialogs';
import type { SettingsForm } from '../form';

/** True to save: always unless the console's detail is staged to "same as the file". */
export async function confirmConsoleDetail(form: SettingsForm): Promise<boolean> {
	const f = form.field('logging.console_detail');
	if (!f.dirty || f.value !== 'file') return true;
	return (
		(await ask({
			title: m.settings_logging_console_title(),
			body: [m.settings_logging_console_body(), m.settings_logging_console_ends()],
			actions: [
				{ id: 'cancel', label: m.settings_form_cancel() },
				{ id: 'on', label: m.settings_logging_console_confirm(), variant: 'primary' }
			],
			initial: 'cancel'
		})) === 'on'
	);
}
