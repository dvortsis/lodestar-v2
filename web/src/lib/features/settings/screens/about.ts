// The purpose lines and the notice of the generated screens (screen.ts), loaded when one opens or
// the Settings filter is first used, so no Settings first view carries them (`Screen.about`).
import { m } from '$lib/paraglide/messages.js';
import type { About } from '../shell/screens';

export const ABOUT: Record<string, About> = {
	crawler: { purpose: m.settings_shell_crawler_purpose },
	network: { purpose: m.settings_shell_network_purpose, notice: m.settings_shell_network_readonly },
	storage: { purpose: m.settings_shell_storage_purpose },
	search: { purpose: m.settings_shell_search_purpose },
	general: { purpose: m.settings_shell_general_purpose }
};
