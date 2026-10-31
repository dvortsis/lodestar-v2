// The choices of the schema's enum-array fields. The form engine reads such a field as a shape it
// doesn't know (the JSON fallback), so the options are named here, in the schema's own order
// (`crates/lodestar/src/settings/{notifications,insights}.rs`): a new option there is an unknown
// checkbox here until it is added, never a wrong value sent.
import { m } from '$lib/paraglide/messages.js';

const CHANNELS = ['ntfy', 'discord', 'email', 'webhook'] as const;

export const CHOICES: Record<string, readonly string[]> = {
	route_alerts: CHANNELS,
	route_updates: CHANNELS,
	route_health: CHANNELS,
	digest_channels: ['email', 'ntfy', 'discord', 'rss']
};

export function channelLabel(id: string): string {
	switch (id) {
		case 'ntfy':
			return 'ntfy';
		case 'discord':
			return 'Discord';
		case 'email':
			return m.settings_screen_channel_email();
		case 'webhook':
			return m.settings_screen_channel_webhook();
		case 'rss':
			return m.settings_screen_channel_rss();
		default:
			return id;
	}
}
