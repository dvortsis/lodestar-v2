<script lang="ts">
	// Settings → Notifications (§10.10.8): ntfy, Discord, email and webhook, each with **Send test**
	// on the values on screen, and which events go where.
	import { m } from '$lib/paraglide/messages.js';
	import type { FieldSpec } from '../form';
	import ChannelsWidget from './ChannelsWidget.svelte';
	import GroupedForm from './GroupedForm.svelte';
	import { flag, given, list, num, str } from './staged';
	import TestButton, { type TestBody } from './TestButton.svelte';

	let { fields }: { fields: FieldSpec[] } = $props();
	const ROUTES = {
		route_alerts: ChannelsWidget,
		route_updates: ChannelsWidget,
		route_health: ChannelsWidget
	};

	type F = Parameters<NonNullable<Parameters<typeof GroupedForm>[1]['setup']>>[0];
	// A secret left out is the saved one, so a test with nothing typed still tests what's saved.
	function body(group: string, form: F): TestBody | null {
		const k = (name: string) => `notifications.${name}` as const;
		switch (group) {
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
	const NAMES: Record<string, () => string> = {
		ntfy: () => 'ntfy',
		discord: () => 'Discord',
		email: () => m.settings_screen_channel_email(),
		webhook: () => m.settings_screen_channel_webhook()
	};
</script>

<GroupedForm id="notifications" {fields} widgets={ROUTES}>
	{#snippet after(group, form)}
		{#if NAMES[group]}
			<TestButton
				label={m.settings_screen_send_test()}
				aria-label={m.settings_screen_send_test_named({ name: NAMES[group]() })}
				missing={group === 'email'
					? m.settings_screen_test_need_email()
					: m.settings_screen_test_need_url()}
				build={() => body(group, form)}
				success={() => m.settings_screen_test_sent()}
			/>
		{/if}
	{/snippet}
</GroupedForm>
