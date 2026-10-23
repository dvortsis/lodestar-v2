// The navigation's group headings, shared by the list and the search results' breadcrumbs.
import { m } from '$lib/paraglide/messages.js';
import type { ScreenGroup } from './registry';

export const groupHeading: Record<ScreenGroup, () => string> = {
	preferences: m.settings_ui_group_preferences,
	crawling: m.settings_ui_group_crawling,
	library: m.settings_ui_group_library,
	connections: m.settings_ui_group_connections,
	system: m.settings_ui_group_system
};
