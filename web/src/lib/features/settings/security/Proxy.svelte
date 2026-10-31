<script lang="ts">
	// Trusted proxy sign-in (§12, B12): a reverse proxy names the signed-in user in a header, and
	// Lodestar believes it only from the trusted addresses. Every save needs the password
	// (§10.10.7). Turning it on loosens who can sign in, so the screen and the dialog both warn
	// first. `LODESTAR_PROXY_AUTH=off` turns it off for the run: the switch shows that, locked, with
	// its source, and the rest can still be saved for when the variable is removed.
	import { ApiError, api } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { confirmWithPassword } from './confirm';
	import { guardLeave } from './guard';
	import { FOCUS_RING } from '$lib/ui/classes';
	import Choice from './Choice.svelte';
	import Result from './Result.svelte';

	// `$lib/ui/fields/Textarea`'s classes (see Choice.svelte for why it isn't imported).
	const AREA = `textarea w-full border-field-border aria-invalid:border-error ${FOCUS_RING}`;
	type Settings = components['schemas']['ProxySettings'];
	type Name = 'trusted' | 'header' | 'allowed_users';

	let saved = $state<Settings | 'failed'>();
	let enabled = $state(false);
	let trusted = $state('');
	let header = $state('');
	let users = $state('');
	let errors = $state<Partial<Record<Name, string>>>({});
	let busy = $state(false);
	let done = $state('');
	let failed = $state(false);

	const lines = (text: string) =>
		text
			.split('\n')
			.map((l) => l.trim())
			.filter(Boolean);

	function fill(s: Settings) {
		saved = s;
		enabled = s.enabled;
		trusted = s.trusted.join('\n');
		header = s.header;
		users = s.allowed_users.join('\n');
	}

	function load() {
		saved = undefined;
		api.get('/auth/proxy').then(fill, () => (saved = 'failed'));
	}
	load();

	const live = $derived(typeof saved === 'object' ? saved : undefined);
	const changed = $derived(
		!!live &&
			(enabled !== live.enabled ||
				lines(trusted).join('\n') !== live.trusted.join('\n') ||
				header.trim() !== live.header ||
				lines(users).join('\n') !== live.allowed_users.join('\n'))
	);
	/** This save turns proxy sign-in on. */
	const turningOn = $derived(!!live && enabled && !live.enabled);
	/** This save lets more addresses or more names sign in: turning it on, a new trusted address,
	 * or a new or emptied allowed list while it's on. */
	const loosening = $derived.by(() => {
		if (!live || !enabled) return false;
		if (!live.enabled) return true;
		const now = lines(users);
		return (
			lines(trusted).some((t) => !live.trusted.includes(t)) ||
			(live.allowed_users.length > 0 &&
				(now.length === 0 || now.some((u) => !live.allowed_users.includes(u))))
		);
	});

	guardLeave({
		dirty: () => changed,
		clear: () => live && fill(live),
		section: () => m.settings_security_proxy_heading()
	});

	async function save(e: SubmitEvent) {
		e.preventDefault();
		if (busy || !live || !changed) return;
		done = '';
		failed = false;
		const body = {
			enabled,
			trusted: lines(trusted),
			header: header.trim() || 'Remote-User',
			allowed_users: lines(users)
		};
		errors =
			enabled && body.trusted.length === 0
				? { trusted: m.settings_security_proxy_trusted_required() }
				: {};
		if (errors.trusted)
			return document.querySelector<HTMLElement>('textarea[name="proxy-trusted"]')?.focus();
		busy = true;
		try {
			const outcome = await confirmWithPassword({
				title: turningOn
					? m.settings_security_proxy_on_title()
					: loosening
						? m.settings_security_proxy_widen_title()
						: m.settings_security_proxy_save_title(),
				body: loosening
					? [
							m.settings_security_proxy_on_body({ trusted: body.trusted.join(', ') }),
							m.settings_security_proxy_safe_port()
						]
					: [m.settings_security_proxy_save_body()],
				action: turningOn
					? m.settings_security_proxy_on_action()
					: m.settings_security_proxy_save_action(),
				// A refused value is the form's to show, not the password's: it closes the dialog.
				send: (password) =>
					api.put('/auth/proxy', { body: { ...body, password } }).then(
						(s) => ({ saved: s }),
						(err: unknown) => {
							if (
								err instanceof ApiError &&
								err.status === 400 &&
								err.field &&
								err.field !== 'password'
							)
								return { invalid: err };
							throw err;
						}
					)
			});
			if (!outcome) return;
			if ('invalid' in outcome) {
				const f = outcome.invalid.field;
				const key: Name = f === 'header' || f === 'allowed_users' ? f : 'trusted';
				errors = { [key]: outcome.invalid.message };
				document
					.querySelector<HTMLElement>(`[name="proxy-${key === 'allowed_users' ? 'users' : key}"]`)
					?.focus();
				return;
			}
			fill(outcome.saved);
			done = m.settings_security_proxy_saved();
		} catch {
			// The dialog's code couldn't load: nothing was sent.
			done = m.settings_security_failed();
			failed = true;
		} finally {
			busy = false;
		}
	}
</script>

<form
	method="post"
	novalidate
	class="flex flex-col gap-4"
	aria-labelledby="security-proxy"
	onsubmit={save}
	data-testid="security-proxy"
>
	<h2 id="security-proxy" class="text-label font-semibold">
		{m.settings_security_proxy_heading()}
	</h2>
	<p class="text-muted-text">{m.settings_security_proxy_what()}</p>
	<p class="text-muted-text">{m.settings_security_proxy_recommend()}</p>
	<div class="text-muted-text">
		<p>{m.settings_security_proxy_safe_heading()}</p>
		<ul class="list-disc pl-5">
			<li>{m.settings_security_proxy_safe_port()}</li>
			<li>{m.settings_security_proxy_safe_ip()}</li>
			<li>{m.settings_security_proxy_safe_gateway()}</li>
		</ul>
	</div>
	{#if saved === 'failed'}
		<p class="flex flex-wrap items-center gap-3">
			{m.settings_security_proxy_failed()}
			<Button onclick={load}>{m.settings_security_retry()}</Button>
		</p>
	{:else if !live}
		<p aria-busy="true" role="status">{m.settings_security_loading()}</p>
	{:else}
		{#if live.forced_off}
			<Banner kind="info" title={m.settings_security_proxy_forced()}>
				<span data-testid="security-proxy-forced">{m.settings_security_proxy_forced_help()}</span>
			</Banner>
		{/if}
		<Choice
			kind="switch"
			bind:checked={enabled}
			label={live.forced_off
				? m.settings_security_proxy_enabled_saved()
				: m.settings_security_proxy_enabled()}
			disabled={busy || live.forced_off}
			data-testid="security-proxy-enabled"
		/>
		{#if loosening}
			<Banner kind="warning" title={m.settings_security_proxy_warn_title()}>
				<span data-testid="security-proxy-warning">{m.settings_security_proxy_warn()}</span>
			</Banner>
		{/if}
		<Field
			label={m.settings_security_proxy_trusted()}
			help={m.settings_security_proxy_trusted_help()}
			error={errors.trusted}
		>
			{#snippet children(props)}
				<textarea
					{...props}
					bind:value={trusted}
					name="proxy-trusted"
					rows={3}
					class="{AREA} font-mono"
					spellcheck="false"
					readonly={busy || undefined}></textarea>
			{/snippet}
		</Field>
		<Field
			label={m.settings_security_proxy_header()}
			help={m.settings_security_proxy_header_help()}
			error={errors.header}
		>
			{#snippet children(props)}
				<Input
					{...props}
					bind:value={header}
					name="proxy-header"
					mono
					autocomplete="off"
					spellcheck="false"
					readonly={busy || undefined}
				/>
			{/snippet}
		</Field>
		<Field
			label={m.settings_security_proxy_users()}
			help={m.settings_security_proxy_users_help()}
			error={errors.allowed_users}
		>
			{#snippet children(props)}
				<textarea
					{...props}
					bind:value={users}
					name="proxy-users"
					rows={2}
					class={AREA}
					spellcheck="false"
					readonly={busy || undefined}></textarea>
			{/snippet}
		</Field>
		<div class="flex flex-wrap items-center gap-3">
			<Button
				type="submit"
				variant="primary"
				loading={busy}
				aria-disabled={!changed || busy || undefined}
				data-testid="security-proxy-save">{m.settings_security_proxy_save()}</Button
			>
			<Result text={done} {failed} testid="security-proxy-result" />
		</div>
	{/if}
</form>
