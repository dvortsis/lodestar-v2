// The Settings screens built by S-settings-2 and S-settings-2b (§10.10.8). Hand-built bodies load
// when the screen opens; Trackers is generated, so it carries no body.
import IconBell from '~icons/ph/bell';
import IconChart from '~icons/ph/chart-line';
import IconLanguage from '~icons/ph/translate';
import IconPlugs from '~icons/ph/plugs-connected';
import IconRadar from '~icons/ph/wifi-high';
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screens: Screen[] = [
	{
		id: 'language',
		group: 'preferences',
		title: () => m.settings_screen_language_title(),
		purpose: () => m.settings_screen_language_purpose(),
		icon: IconLanguage,
		load: () => import('./LanguageScreen.svelte')
	},
	// Per-tracker health has no endpoint of its own (`GET /insights/index` reports it, rolled up),
	// so this screen shows the settings only; `extra` is where a health panel would mount.
	{
		id: 'trackers',
		group: 'library',
		title: () => m.settings_screen_trackers_title(),
		purpose: () => m.settings_screen_trackers_purpose(),
		icon: IconRadar,
		sections: ['trackers']
	},
	// A hand-built screen's purpose is its body's heading line (lazy), so it isn't carried here.
	{
		id: 'integrations',
		group: 'connections',
		title: () => m.settings_screen_integrations_title(),
		icon: IconPlugs,
		load: () => import('./IntegrationsScreen.svelte')
	},
	{
		id: 'notifications',
		group: 'connections',
		title: () => m.settings_screen_notifications_title(),
		icon: IconBell,
		load: () => import('./NotificationsScreen.svelte')
	},
	{
		id: 'insights',
		group: 'connections',
		title: () => m.settings_screen_insights_title(),
		icon: IconChart,
		load: () => import('./InsightsScreen.svelte')
	}
];

export default screens;
