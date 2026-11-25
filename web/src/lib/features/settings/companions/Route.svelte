<script lang="ts">
	// A companion's route editor (X-cmp-egress-ui): only the kinds the API takes (`direct`,
	// `socks5`), saved behind the shared admin-password confirmation (Security's, as every risky
	// change uses it). Saving restarts the companion; the answer says only whether that restart began,
	// so nothing here ever says "applied". A refusal is a sentence per code, naming the field, never
	// the value. The password lives only in the dialog's field.
	import { onDestroy, untrack } from 'svelte';
	import { ApiError } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import {
		egressWords,
		notRestartedWords,
		passwordRefusal,
		routeText,
		type EgressChanged,
		type EgressSetting
	} from './egress';
	import { wordsFor, type Companion } from './logic';
	import type { CompanionsStore } from './store.svelte';

	interface Props {
		store: CompanionsStore;
		c: Companion;
		/** The kind chosen and not yet saved, for the row's live SOCKS5 warnings. */
		draft?: '' | 'direct' | 'socks5';
		/** Saved: the sentence the row shows once this panel closes. */
		ondone: (note: string) => void;
		/**
		 * Refused by the server: the row shows the sentence, so it stays if the refreshed row has no
		 * route any more and this panel goes. Empty clears it.
		 */
		onrefused: (text: string) => void;
	}
	let { store, c, draft = $bindable(''), ondone, onrefused }: Props = $props();

	type Flows = typeof import('../security/confirm');
	let flows: Flows | undefined;
	onDestroy(() => flows?.dismissPasswordDialogs());

	// Starts from the saved route when it's one offered here; `interface` and `bind_address` aren't.
	const fill = (s: EgressSetting | null) => ({
		route: s?.route === 'direct' || s?.route === 'socks5' ? s.route : ('' as const),
		proxy: s?.route === 'socks5' ? (s.proxy ?? '') : ''
	});
	/** What the form was last filled with: the form is untouched while it still shows this. */
	let filled = fill(untrack(() => c.egress?.setting ?? null));
	let route = $state<'' | 'direct' | 'socks5'>(filled.route);
	let proxy = $state(filled.proxy);
	let busy = $state(false);
	let error = $state('');
	$effect(() => {
		draft = route;
	});
	// The server's route changed while this is open (another tab, a refresh after "busy"): an
	// untouched form follows it, so a Save can't send the older route back over the newer one. A
	// form the user has changed keeps their choice; it's measured against the newer route from then on.
	$effect(() => {
		const next = fill(c.egress?.setting ?? null);
		untrack(() => {
			if (route === filled.route && proxy === filled.proxy) {
				route = next.route;
				proxy = next.proxy;
			}
			filled = next;
		});
	});

	// A companion's SOCKS5 launch always has the proxy resolve names (`socks5://`, manager.rs
	// `launch_env`), so the source's DNS choice isn't offered; `proxy` says what it does.
	const setting = (): EgressSetting =>
		route === 'socks5' ? { route, proxy: proxy.trim(), dns: 'proxy' } : { route: 'direct' };

	async function save(e: Event) {
		e.preventDefault();
		if (busy) return;
		error = '';
		onrefused(''); // a new try clears the row's last refusal
		if (route === '') return void (error = m.settings_companions_route_choose_first());
		if (route === 'socks5' && proxy.trim() === '')
			return void (error = m.settings_companions_route_proxy_missing());
		const egress = setting();
		busy = true;
		try {
			flows ??= await import('../security/confirm');
			const r = await flows.confirmWithPassword<{ ok: EgressChanged } | { refused: unknown }>({
				title: m.settings_companions_route_confirm_title({ name: c.name }),
				body: [m.settings_companions_route_restart_warning({ name: c.name })],
				action: m.settings_companions_route_save(),
				send: async (password) => {
					try {
						return { ok: await store.setEgress(c.id, egress, password) };
					} catch (x) {
						// Password refusals stay in the dialog, in this screen's words; the rest close it.
						if (passwordRefusal(x)) {
							const a = x as ApiError;
							throw new ApiError(a.status, a.code, wordsFor(a.code), { retryAfter: a.retryAfter });
						}
						return { refused: x };
					}
				}
			});
			// Null is Cancel, or this editor unmounting (the row lost its route, the screen closed):
			// onDestroy dismisses the dialog, which resolves null even if the PUT had already gone
			// out. That save may still apply; the store's refresh shows the row's route either way,
			// so nothing here claims it was or wasn't saved.
			if (r === null) return;
			if ('refused' in r) {
				onrefused(egressWords(r.refused));
				await store.refresh(c.id);
				return;
			}
			ondone(
				r.ok.restarting
					? m.settings_companions_route_restarting({ name: c.name })
					: `${m.settings_companions_route_saved_later({ name: c.name })} ${notRestartedWords(r.ok.not_restarted)}`
			);
		} catch (x) {
			onrefused(egressWords(x));
		} finally {
			busy = false;
		}
	}
</script>

<form class="flex flex-col gap-3" data-route-editor={c.id} onsubmit={save}>
	{#if c.egress}
		<p class="text-small" data-route={c.egress.from}>
			{routeText(c.egress.setting)}
			{#if c.egress.setting}
				<span class="text-muted-text"
					>{c.egress.from === 'saved'
						? m.settings_companions_route_from_saved()
						: m.settings_companions_route_from_source()}</span
				>
			{/if}
		</p>
	{/if}
	<Field label={m.settings_companions_route_kind()}>
		{#snippet children(props)}
			<select {...props} bind:value={route} class="select w-full border-field-border">
				<option value="" disabled>{m.settings_companions_route_kind_choose()}</option>
				<option value="direct">{m.settings_companions_route_kind_direct()}</option>
				<option value="socks5">{m.settings_companions_route_kind_socks5()}</option>
			</select>
		{/snippet}
	</Field>
	{#if route === 'direct'}
		<div data-direct-warning>
			<Banner kind="warning">{m.settings_companions_route_direct_warning()}</Banner>
		</div>
	{:else if route === 'socks5'}
		<Field
			label={m.settings_companions_route_proxy()}
			help={m.settings_companions_route_proxy_help()}
		>
			{#snippet children(props)}
				<Input {...props} bind:value={proxy} autocomplete="off" spellcheck="false" />
			{/snippet}
		</Field>
	{/if}
	<p class="text-small" id="{c.id}-route-restart" data-restart-warning>
		{m.settings_companions_route_restart_warning({ name: c.name })}
	</p>
	<div>
		<Button type="submit" variant="primary" loading={busy} aria-describedby="{c.id}-route-restart"
			>{m.settings_companions_route_save()}</Button
		>
	</div>
	{#if error}<Banner kind="error">{error}</Banner>{/if}
</form>
