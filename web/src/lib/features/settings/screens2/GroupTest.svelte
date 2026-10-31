<script lang="ts">
	// The test button of a group on a connection screen (§10.10.8): Test connection for a Torznab
	// endpoint, Send test for a notification channel. It posts the values on screen. A secret left
	// out is the saved one (B-settings-sections part b), so nothing here asks for a saved secret.
	import { m } from '$lib/paraglide/messages.js';
	import type { SettingsForm } from '../form';
	import { channelLabel } from './channels';
	import { flag, given, list, num, str } from './staged';
	import TestButton, { type TestBody } from './TestButton.svelte';

	let { form, group }: { form: SettingsForm; group: string } = $props();

	function body(): TestBody | null {
		const k = (name: string) => `notifications.${name}` as const;
		switch (group) {
			case 'torznab': {
				const url = str(form, 'integrations.torznab_url').trim();
				return url === ''
					? null
					: {
							kind: 'torznab',
							url,
							...given('api_key', str(form, 'integrations.torznab_api_key'))
						};
			}
			case 'ntfy':
				return {
					kind: 'ntfy',
					...given('url', str(form, k('ntfy_url'))),
					...given('token', str(form, k('ntfy_token'))),
					priority: num(form, k('ntfy_priority')),
					allow_private: flag(form, k('ntfy_allow_private'))
				};
			case 'discord':
				return { kind: 'discord', ...given('url', str(form, k('discord_url'))) };
			case 'webhook':
				return {
					kind: 'webhook',
					...given('url', str(form, k('webhook_url'))),
					allow_private: flag(form, k('webhook_allow_private'))
				};
			case 'email': {
				const host = str(form, k('email_host')).trim();
				const from = str(form, k('email_from')).trim();
				const to = list(form, k('email_to'));
				if (!host || !from || !to.length) return null;
				return {
					kind: 'email',
					host,
					from,
					to,
					port: num(form, k('email_port')),
					security: str(form, k('email_security')) as 'tls',
					username: str(form, k('email_username')),
					...given('password', str(form, k('email_password'))),
					allow_private: flag(form, k('email_allow_private'))
				};
			}
			default:
				return null;
		}
	}
	const torznab = $derived(group === 'torznab');
	const name = $derived(torznab ? 'Torznab' : channelLabel(group));
</script>

<TestButton
	label={torznab ? m.settings_screen_test_connection() : m.settings_screen_send_test()}
	aria-label={torznab
		? m.settings_screen_test_connection_named({ name })
		: m.settings_screen_send_test_named({ name })}
	missing={group === 'email'
		? m.settings_screen_test_need_email()
		: m.settings_screen_test_need_url()}
	build={body}
	success={(version) =>
		torznab
			? version
				? m.settings_screen_test_connected_version({ version })
				: m.settings_screen_test_connected()
			: m.settings_screen_test_sent()}
/>
