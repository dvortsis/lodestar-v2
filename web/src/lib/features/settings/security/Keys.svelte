<script lang="ts">
	// API keys and feed tokens (§10.10.8, B12): the list, New, Regenerate and Revoke. Creating and
	// regenerating need the password (§10.10.7); revoking only takes access away, so it asks a plain
	// question, as the server does (B12). A new secret goes from the answer straight into the
	// reveal dialog (reveal.ts) and is never kept here.
	import { onDestroy } from 'svelte';
	import { ApiError, api } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { ask } from '../form/dialogs';
	import { confirmWithPassword } from './confirm';
	import { guardLeave } from './guard';
	import { dismissReveals, loadReveal, revealSecret } from './reveal';
	import Choice from './Choice.svelte';
	import { keyDate } from './date';
	import Result from './Result.svelte';
	import { statusIcons } from '$lib/ui/status';
	import IconWarning from '~icons/ph/warning';

	type Key = components['schemas']['KeyInfo'];
	type Kind = components['schemas']['KeyKind'];
	type Scope = components['schemas']['KeyScope'];
	const SCOPES: Scope[] = ['search', 'torznab', 'feeds'];

	let keys = $state<Key[] | 'failed'>();
	let name = $state('');
	let kind = $state<Kind>('api_key');
	let scopes = $state<Record<Scope, boolean>>({ search: true, torznab: false, feeds: false });
	let errors = $state<{ name?: string; scopes?: string }>({});
	let busy = $state<string>();
	let done = $state('');
	let failed = $state(false);

	function load() {
		keys = undefined;
		api.get('/keys').then(
			(r) => (keys = r.keys),
			() => (keys = 'failed')
		);
	}
	load();
	onDestroy(dismissReveals);

	const kindLabel = (k: Kind) =>
		k === 'api_key' ? m.settings_security_keys_kind_api() : m.settings_security_keys_kind_feed();
	const scopeLabel = (s: Scope) =>
		s === 'search'
			? m.settings_security_keys_scope_search()
			: s === 'torznab'
				? m.settings_security_keys_scope_torznab()
				: m.settings_security_keys_scope_feeds();
	const statusIcon = (k: Key) =>
		statusIcons[k.status === 'active' ? 'success' : k.status === 'expired' ? 'warning' : 'error'];
	const statusLabel = (k: Key) =>
		k.status === 'active'
			? m.settings_security_keys_active()
			: k.status === 'expired'
				? m.settings_security_keys_expired()
				: m.settings_security_keys_revoked();
	const date = (seconds: number) => keyDate(seconds, currentLocale());

	function clear() {
		name = '';
		errors = {};
	}
	guardLeave({
		dirty: () => name.trim() !== '',
		clear,
		section: () => m.settings_security_keys_new()
	});

	function report(text: string, bad = false) {
		done = text;
		failed = bad;
	}

	/** Runs one action with the busy mark on its button. A dialog that can't load sent nothing:
	 * the reveal is loaded before the password is asked for. */
	async function run(id: string, action: () => Promise<void>) {
		if (busy) return;
		busy = id;
		report('');
		try {
			await action();
		} catch {
			report(m.settings_security_failed(), true);
		} finally {
			busy = undefined;
		}
	}

	type Created = components['schemas']['CreatedKey'];
	/** A refusal that isn't the password's (a bad name, too many keys, a key no longer active)
	 * closes the dialog and shows on the screen, so the password isn't blamed for it. */
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
		if (err.field === 'name') errors = { name: err.message };
		else if (err.field === 'scopes') errors = { scopes: err.message };
		else report(m.settings_security_keys_key_failed({ reason: err.message }), true);
	}

	function create(e: SubmitEvent) {
		e.preventDefault();
		const title = name.trim();
		const chosen = SCOPES.filter((s) => scopes[s]);
		errors = {
			name: !title
				? m.settings_security_keys_name_required()
				: [...title].length > 200
					? m.settings_security_keys_name_long()
					: undefined,
			scopes:
				kind === 'api_key' && chosen.length === 0
					? m.settings_security_keys_scopes_required()
					: undefined
		};
		if (errors.name || errors.scopes) {
			document
				.querySelector<HTMLInputElement>(
					errors.name ? 'input[name="key-name"]' : 'input[name="key-scope"]'
				)
				?.focus();
			return;
		}
		void run('create', async () => {
			await loadReveal();
			const outcome = await confirmWithPassword({
				title: m.settings_security_keys_create_title({ name: title }),
				body: [m.settings_security_keys_create_body()],
				action: m.settings_security_keys_create_action(),
				send: (password) =>
					answer(
						api.post('/keys', {
							body: {
								name: title,
								kind,
								...(kind === 'api_key' ? { scopes: chosen } : {}),
								password
							}
						})
					)
			});
			if (!outcome) return;
			if ('invalid' in outcome) return refused(outcome.invalid);
			const { made } = outcome;
			clear();
			if (Array.isArray(keys)) keys = [...keys, made.key];
			report(m.settings_security_keys_created({ name: made.key.name }));
			void revealSecret(made.key.name, made.secret);
		});
	}

	function regenerate(key: Key) {
		void run(`regenerate-${key.id}`, async () => {
			await loadReveal();
			const outcome = await confirmWithPassword({
				title: m.settings_security_keys_regenerate_title({ name: key.name }),
				body: [m.settings_security_keys_regenerate_body()],
				action: m.settings_security_keys_regenerate_action(),
				send: (password) =>
					answer(api.post('/keys/{id}/regenerate', { path: { id: key.id }, body: { password } }))
			});
			if (!outcome) return;
			if ('invalid' in outcome) return refused(outcome.invalid);
			const { made } = outcome;
			// The new secret has a new id and prefix; the old row is gone.
			if (Array.isArray(keys)) keys = keys.map((k) => (k.id === key.id ? made.key : k));
			report(m.settings_security_keys_regenerated({ name: key.name }));
			void revealSecret(made.key.name, made.secret);
		});
	}

	function revoke(key: Key) {
		void run(`revoke-${key.id}`, async () => {
			const answer = await ask({
				title: m.settings_security_keys_revoke_title({ name: key.name }),
				body: [m.settings_security_keys_revoke_body()],
				actions: [
					{ id: 'keep', label: m.settings_security_keys_keep() },
					{ id: 'revoke', label: m.settings_security_keys_revoke_action(), variant: 'danger' }
				],
				initial: 'keep'
			});
			if (answer !== 'revoke') return;
			const gone = await api.post('/keys/{id}/revoke', { path: { id: key.id }, body: {} });
			if (Array.isArray(keys)) keys = keys.map((k) => (k.id === key.id ? gone : k));
			report(m.settings_security_keys_revoked_done({ name: key.name }));
		});
	}
</script>

<div class="flex flex-col gap-4" data-testid="security-keys">
	<p class="text-muted-text">{m.settings_security_keys_help()}</p>
	{#if keys === 'failed'}
		<p class="flex flex-wrap items-center gap-3">
			{m.settings_security_keys_failed()}
			<Button onclick={load}>{m.settings_security_retry()}</Button>
		</p>
	{:else if !keys}
		<p aria-busy="true" role="status">{m.settings_security_loading()}</p>
	{:else if keys.length === 0}
		<p data-testid="security-keys-empty">{m.settings_security_keys_empty()}</p>
	{:else}
		<ul class="flex flex-col gap-3">
			{#each keys as key (key.id)}
				{@const Icon = statusIcon(key)}
				<li
					class="flex flex-wrap items-center gap-3 rounded-box border border-base-300 p-3"
					data-testid="security-key"
				>
					<div class="flex min-w-0 flex-1 flex-col gap-1">
						<p class="font-semibold">{key.name}</p>
						<p class="flex flex-wrap items-center gap-1 text-small text-muted-text">
							{kindLabel(key.kind)} · <span class="font-mono">{key.prefix}…</span> ·
							<Icon aria-hidden="true" />{statusLabel(key)}
						</p>
						<p class="text-small text-muted-text">
							{#if key.kind === 'api_key'}{key.scopes.map(scopeLabel).join(', ')} ·
							{/if}<span data-testid="security-key-created"
								>{m.settings_security_keys_created_at({ date: date(key.created_at) })}</span
							>
							·
							<span data-testid="security-key-used"
								>{key.last_used_at === null
									? m.settings_security_keys_never_used()
									: m.settings_security_keys_used_at({ date: date(key.last_used_at) })}</span
							>
						</p>
					</div>
					{#if key.status === 'active'}
						<div class="flex flex-wrap gap-2">
							<Button
								loading={busy === `regenerate-${key.id}`}
								aria-label={m.settings_security_keys_regenerate_named({ name: key.name })}
								onclick={() => regenerate(key)}>{m.settings_security_keys_regenerate()}</Button
							>
							<Button
								loading={busy === `revoke-${key.id}`}
								aria-label={m.settings_security_keys_revoke_named({ name: key.name })}
								onclick={() => revoke(key)}>{m.settings_security_keys_revoke()}</Button
							>
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
	<Result text={done} {failed} testid="security-keys-result" />
	<form
		method="post"
		novalidate
		class="flex flex-col gap-4"
		aria-labelledby="security-key-new"
		onsubmit={create}
	>
		<h3 id="security-key-new" class="font-semibold">{m.settings_security_keys_new()}</h3>
		<Field
			label={m.settings_security_keys_name()}
			help={m.settings_security_keys_name_help()}
			error={errors.name}
			required
		>
			{#snippet children(props)}
				<Input {...props} bind:value={name} name="key-name" autocomplete="off" maxlength={200} />
			{/snippet}
		</Field>
		<fieldset class="flex flex-col gap-2">
			<legend class="mb-2">{m.settings_security_keys_kind()}</legend>
			<Choice
				kind="radio"
				bind:group={kind}
				name="key-kind"
				value="api_key"
				label={m.settings_security_keys_kind_api_help()}
			/>
			<Choice
				kind="radio"
				bind:group={kind}
				name="key-kind"
				value="feed_token"
				label={m.settings_security_keys_kind_feed_help()}
			/>
		</fieldset>
		{#if kind === 'api_key'}
			<fieldset
				class="flex flex-col gap-2"
				aria-describedby={errors.scopes ? 'security-key-scopes-error' : undefined}
			>
				<legend class="mb-2">{m.settings_security_keys_scopes()}</legend>
				{#each SCOPES as s (s)}
					<Choice
						kind="checkbox"
						bind:checked={scopes[s]}
						name="key-scope"
						label={scopeLabel(s)}
						aria-invalid={errors.scopes ? true : undefined}
					/>
				{/each}
				{#if errors.scopes}
					<p id="security-key-scopes-error" class="flex items-start gap-1 text-small">
						<IconWarning class="mt-0.5 size-4 shrink-0 text-error" aria-hidden="true" />
						<span>{errors.scopes}</span>
					</p>
				{/if}
			</fieldset>
		{/if}
		<div>
			<Button
				type="submit"
				variant="primary"
				loading={busy === 'create'}
				data-testid="security-key-create">{m.settings_security_keys_create()}</Button
			>
		</div>
	</form>
</div>
