<script lang="ts">
	// Torznab for Prowlarr, Sonarr and Radarr (TZ-0): the address with Copy, the keys with the
	// `torznab` scope, and New / Regenerate / Revoke. B12 keeps only a key's hash, so "reveal" is the
	// one showing at create or regenerate, behind the password (§10.10.7), in Security's own reveal
	// dialog, unchanged (runner, 2026-11-12; decisions file). The secret goes from the answer
	// straight into that dialog's props and is never kept here.
	import { onDestroy } from 'svelte';
	import { resolve } from '$app/paths';
	import { ApiError, api } from '$lib/api/client';
	import { formatDate } from '$lib/format';
	import { currentLocale, displayTimeZone } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { statusIcons } from '$lib/ui/status';
	import { copyText } from '$lib/util/clipboard';
	import { ask } from '../form/dialogs';
	import Result from './Status.svelte';
	import { nameError, suggestName, torznabKeys, torznabUrl, wider, type Key } from './logic';
	import type { components } from '$lib/api/schema';

	type Flows = typeof import('./key-flows');
	let flows = $state<Flows>();
	/** Security's dialogs, loaded on first use; a failure to load means nothing is sent. */
	const loadFlows = async () => (flows ??= await import('./key-flows'));

	const url = torznabUrl(location.origin);
	let keys = $state<Key[] | 'failed'>();
	let name = $state('');
	let nameErr = $state<string>();
	let busy = $state<string>();
	let done = $state('');
	let failed = $state(false);
	let copied = $state<'' | 'done' | 'failed'>('');

	function load() {
		keys = undefined;
		api.get('/keys').then(
			(r) => {
				keys = torznabKeys(r.keys);
				if (!name) name = suggestName(keys);
			},
			() => (keys = 'failed')
		);
	}
	load();
	// Set false on unmount: an action still loading its dialogs then stops instead of mounting one
	// over another page.
	let live = true;
	onDestroy(() => {
		live = false;
		flows?.dismissPasswordDialogs();
		flows?.dismissReveals();
	});

	async function copyUrl() {
		if ((await copyText(url)) === 'copied') copied = 'done';
		else {
			copied = 'failed';
			document.querySelector<HTMLInputElement>('[data-testid="outputs-torznab-url"]')?.select();
		}
	}

	const statusIcon = (k: Key) =>
		statusIcons[k.status === 'active' ? 'success' : k.status === 'expired' ? 'warning' : 'error'];
	const statusLabel = (k: Key) =>
		k.status === 'active'
			? m.settings_outputs_keys_active()
			: k.status === 'expired'
				? m.settings_outputs_keys_expired()
				: m.settings_outputs_keys_revoked();
	const date = (seconds: number) =>
		formatDate(seconds, currentLocale(), { timeZone: displayTimeZone() || undefined });

	function report(text: string, bad = false) {
		done = text;
		failed = bad;
	}

	/** Runs one action with the busy mark on its button. */
	async function run(id: string, action: () => Promise<void>) {
		if (busy) return;
		busy = id;
		report('');
		try {
			await action();
		} catch {
			report(m.settings_outputs_failed(), true);
		} finally {
			busy = undefined;
		}
	}

	type Created = components['schemas']['CreatedKey'];
	/** A refusal that isn't the password's closes the dialog and shows on the screen. */
	const answer = (sent: Promise<Created>) =>
		sent.then(
			(made) => ({ made }),
			(err: unknown) => {
				if (
					err instanceof ApiError &&
					err.field !== 'password' &&
					(err.status === 400 || err.status === 409)
				)
					return { invalid: err };
				throw err;
			}
		);

	function refused(err: ApiError) {
		if (err.field === 'name') nameErr = err.message;
		else report(m.settings_outputs_key_failed({ reason: err.message }), true);
	}

	function create(e: SubmitEvent) {
		e.preventDefault();
		const title = name.trim();
		const bad = nameError(title);
		nameErr =
			bad === 'required'
				? m.settings_outputs_new_name_required()
				: bad === 'long'
					? m.settings_outputs_new_name_long()
					: undefined;
		if (nameErr) {
			document.querySelector<HTMLInputElement>('input[name="torznab-key-name"]')?.focus();
			return;
		}
		void run('create', async () => {
			// Loaded before the password is asked for: a dialog that can't load means nothing is made.
			const { confirmWithPassword, loadReveal, revealSecret } = await loadFlows();
			await loadReveal();
			if (!live) return;
			const outcome = await confirmWithPassword({
				title: m.settings_outputs_create_title({ name: title }),
				body: [m.settings_outputs_create_body({ name: title })],
				action: m.settings_outputs_create_action(),
				send: (password) =>
					answer(
						api.post('/keys', {
							body: { name: title, kind: 'api_key', scopes: ['torznab'], password }
						})
					)
			});
			if (!outcome || !live) return;
			if ('invalid' in outcome) return refused(outcome.invalid);
			const { made } = outcome;
			if (Array.isArray(keys)) {
				keys = [...keys, made.key];
				name = suggestName(keys);
			}
			report(m.settings_outputs_created({ name: made.key.name }));
			void revealSecret(made.key.name, made.secret);
		});
	}

	function regenerate(key: Key) {
		void run(`regenerate-${key.id}`, async () => {
			const { confirmWithPassword, loadReveal, revealSecret } = await loadFlows();
			await loadReveal();
			if (!live) return;
			const outcome = await confirmWithPassword({
				title: m.settings_outputs_regenerate_title({ name: key.name }),
				body: [m.settings_outputs_regenerate_body({ name: key.name })],
				action: m.settings_outputs_regenerate_action(),
				send: (password) =>
					answer(api.post('/keys/{id}/regenerate', { path: { id: key.id }, body: { password } }))
			});
			if (!outcome || !live) return;
			if ('invalid' in outcome) return refused(outcome.invalid);
			const { made } = outcome;
			// The new secret has a new id and prefix; the old row is gone.
			if (Array.isArray(keys)) keys = keys.map((k) => (k.id === key.id ? made.key : k));
			report(m.settings_outputs_regenerated({ name: key.name }));
			void revealSecret(made.key.name, made.secret);
		});
	}

	function revoke(key: Key) {
		void run(`revoke-${key.id}`, async () => {
			const choice = await ask({
				title: m.settings_outputs_revoke_title({ name: key.name }),
				body: [m.settings_outputs_revoke_body({ name: key.name })],
				actions: [
					{ id: 'keep', label: m.settings_outputs_keep() },
					{ id: 'revoke', label: m.settings_outputs_revoke_action(), variant: 'danger' }
				],
				initial: 'keep'
			});
			if (choice !== 'revoke') return;
			const gone = await api.post('/keys/{id}/revoke', { path: { id: key.id }, body: {} });
			if (Array.isArray(keys)) keys = keys.map((k) => (k.id === key.id ? gone : k));
			report(m.settings_outputs_revoked_done({ name: key.name }));
		});
	}
</script>

<div class="flex flex-col gap-4" data-testid="outputs-torznab">
	<p class="text-muted-text">{m.settings_outputs_torznab_intro()}</p>
	<Field label={m.settings_outputs_torznab_url()} help={m.settings_outputs_torznab_url_help()}>
		{#snippet children(props)}
			<div class="flex flex-wrap items-center gap-2">
				<Input
					{...props}
					value={url}
					mono
					readonly
					spellcheck="false"
					class="min-w-0 flex-1"
					onfocus={(e) => e.currentTarget.select()}
					data-testid="outputs-torznab-url"
				/>
				<Button onclick={copyUrl} data-testid="outputs-torznab-url-copy"
					>{m.settings_outputs_copy()}</Button
				>
			</div>
		{/snippet}
	</Field>
	<Result
		text={copied === 'done'
			? m.settings_outputs_copied()
			: copied === 'failed'
				? m.settings_outputs_copy_failed()
				: ''}
		failed={copied === 'failed'}
		testid="outputs-torznab-url-copied"
	/>
	<p class="text-small text-muted-text">{m.settings_outputs_torznab_categories()}</p>

	<h3 class="font-semibold">{m.settings_outputs_keys_heading()}</h3>
	<p class="text-muted-text">{m.settings_outputs_keys_help()}</p>
	{#if keys === 'failed'}
		<p class="flex flex-wrap items-center gap-3">
			{m.settings_outputs_keys_failed()}
			<Button onclick={load}>{m.settings_outputs_retry()}</Button>
		</p>
	{:else if !keys}
		<p aria-busy="true" role="status">{m.settings_outputs_loading()}</p>
	{:else if keys.length === 0}
		<p data-testid="outputs-keys-empty">{m.settings_outputs_keys_empty()}</p>
	{:else}
		<ul class="flex flex-col gap-3">
			{#each keys as key (key.id)}
				{@const Icon = statusIcon(key)}
				<li
					class="flex flex-wrap items-center gap-3 rounded-box border border-base-300 p-3"
					data-testid="outputs-key"
				>
					<div class="flex min-w-0 flex-1 flex-col gap-1">
						<p class="font-semibold">{key.name}</p>
						<p class="flex flex-wrap items-center gap-1 text-small text-muted-text">
							<span class="font-mono">{key.prefix}…</span> ·
							<Icon aria-hidden="true" />{statusLabel(key)}
						</p>
						<p class="text-small text-muted-text">
							<span data-testid="outputs-key-created"
								>{m.settings_outputs_keys_created_at({ date: date(key.created_at) })}</span
							>
							·
							<span data-testid="outputs-key-used"
								>{key.last_used_at === null
									? m.settings_outputs_keys_never_used()
									: m.settings_outputs_keys_used_at({ date: date(key.last_used_at) })}</span
							>
						</p>
						{#if wider(key)}
							<p class="text-small" data-testid="outputs-key-wider">
								<span class="font-semibold">{m.settings_outputs_keys_wider()}</span>
								{m.settings_outputs_keys_wider_help()}
							</p>
						{/if}
					</div>
					{#if key.status === 'active'}
						<div class="flex flex-wrap gap-2">
							<Button
								loading={busy === `regenerate-${key.id}`}
								aria-label={m.settings_outputs_regenerate_named({ name: key.name })}
								onclick={() => regenerate(key)}>{m.settings_outputs_regenerate()}</Button
							>
							<Button
								loading={busy === `revoke-${key.id}`}
								aria-label={m.settings_outputs_revoke_named({ name: key.name })}
								onclick={() => revoke(key)}>{m.settings_outputs_revoke()}</Button
							>
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
	<Result text={done} {failed} testid="outputs-keys-result" />
	<form
		method="post"
		novalidate
		class="flex flex-col gap-4"
		aria-labelledby="outputs-key-new"
		onsubmit={create}
	>
		<h3 id="outputs-key-new" class="font-semibold">{m.settings_outputs_new_heading()}</h3>
		<Field
			label={m.settings_outputs_new_name()}
			help={m.settings_outputs_new_name_help()}
			error={nameErr}
			required
		>
			{#snippet children(props)}
				<Input
					{...props}
					bind:value={name}
					name="torznab-key-name"
					autocomplete="off"
					maxlength={200}
				/>
			{/snippet}
		</Field>
		<div>
			<Button
				type="submit"
				variant="primary"
				loading={busy === 'create'}
				data-testid="outputs-key-create">{m.settings_outputs_new_create()}</Button
			>
		</div>
	</form>
	<p class="text-small text-muted-text">
		{m.settings_outputs_keys_security()}
		<a class="link ls-focus" href={resolve('/settings/[section]', { section: 'security' })}
			>{m.settings_outputs_keys_security_link()}</a
		>
	</p>
</div>
