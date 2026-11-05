<script lang="ts">
	// First-run setup (§10.10.10 steps 1 and 2, with the Legal Notice between them, owner decision
	// §2.38): the setup token from the log, the notice and its required checkbox, then the admin
	// password. The server takes all three in one request (POST /setup), so the earlier steps only
	// check locally; a wrong token comes back from the submit and returns to step 1, and a notice
	// that changed meanwhile returns to step 2 with the new one.
	import { onDestroy, tick } from 'svelte';
	import { api } from '$lib/api/client';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import IconWarning from '~icons/ph/warning-circle';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { failureMessage } from './errors';
	import { createAuthForm } from './form.svelte';
	import FormAlert from './FormAlert.svelte';
	import NoticeBox from './NoticeBox.svelte';
	import { resetNoticeCache, type LegalNotice } from './notice';
	import type { FieldName } from './machine';
	import { toLogin, toWelcome } from './nav';
	import { checkPassword, isBlocking, MIN_PASSWORD_CHARS } from './password';
	import { setSession } from './session.svelte';
	import { parseToken, TOKEN_HINT } from './token';

	const form = createAuthForm();
	onDestroy(form.destroy);

	type Step = 'token' | 'notice' | 'password';
	let step = $state<Step>('token');
	let notice = $state<LegalNotice | null>(null);
	let read = $state(false);
	let token = $state('');
	let password = $state('');
	let confirm = $state('');
	let root = $state<HTMLDivElement>();

	const steps: { id: Step; label: () => string }[] = [
		{ id: 'token', label: m.auth_setup_step_token },
		{ id: 'notice', label: m.auth_setup_step_notice },
		{ id: 'password', label: m.auth_setup_step_password }
	];

	const strength = $derived(password ? checkPassword(password) : null);
	const strengthHint = $derived(
		strength === 'weak'
			? m.auth_strength_weak()
			: strength === 'fair'
				? m.auth_strength_fair()
				: strength === 'strong'
					? m.auth_strength_strong()
					: ''
	);
	const errorFor = (field: FieldName) =>
		form.state.name === 'error' && form.state.failure.field === field
			? failureMessage(form.state.failure)
			: undefined;
	const locked = $derived(form.state.name === 'locked');
	// The notice's two messages live here, not in errors.ts, which other routes load.
	const noticeError = $derived(
		form.state.name !== 'error' || form.state.failure.field !== 'notice'
			? undefined
			: form.state.failure.kind === 'invalid'
				? m.auth_error_notice_unchecked()
				: m.auth_error_notice_changed()
	);

	async function focus(name: FieldName, select = false) {
		await tick();
		const input = root?.querySelector<HTMLInputElement>(`input[name="${name}"]`);
		input?.focus();
		if (select) input?.select();
	}

	function invalid(field: FieldName, reason: string) {
		form.invalid({ kind: 'invalid', field, reason });
		return focus(field, true);
	}

	async function continueToNotice(event: SubmitEvent) {
		event.preventDefault();
		const parsed = parseToken(token);
		if (!parsed) return invalid('token', 'token_invalid');
		token = parsed;
		form.edit();
		step = 'notice';
		focusNotice = true;
	}

	// Entering the notice step puts focus on the notice itself, once it has loaded, so a keyboard
	// user can scroll it and then Tab to the checkbox.
	let focusNotice = false;
	$effect(() => {
		if (step !== 'notice' || !notice || !focusNotice) return;
		focusNotice = false;
		void tick().then(() =>
			root?.querySelector<HTMLElement>('[data-testid="legal-notice"]')?.focus()
		);
	});

	async function continueToPassword(event: SubmitEvent) {
		event.preventDefault();
		if (!notice) return;
		if (!read) return invalid('notice', 'notice_unchecked');
		form.edit();
		step = 'password';
		await focus('password');
	}

	async function back() {
		form.edit();
		step = step === 'password' ? 'notice' : 'token';
		await focus(step);
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (form.busy || locked) return;
		const check = checkPassword(password);
		if (isBlocking(check)) return invalid('password', check);
		if (confirm !== password) return invalid('confirm', 'mismatch');
		if (!notice || !read) {
			step = 'notice';
			return invalid('notice', 'notice_unchecked');
		}
		const body = { token, password, notice_version: notice.version };
		const ok = await form.submit(() => api.post('/setup', { body }));
		if (ok) {
			token = password = confirm = '';
			setSession('signed_in');
			await toWelcome();
			return;
		}
		const s = form.state;
		if (s.name !== 'error') return;
		if (s.failure.kind === 'setup_done') {
			setSession('signed_out');
			await toLogin();
		} else if (s.failure.kind === 'bad_token') {
			step = 'token';
			await focus('token', true);
		} else if (s.failure.kind === 'notice_required') {
			// A newer build is serving: show its notice and ask again.
			resetNoticeCache();
			notice = null;
			read = false;
			step = 'notice';
		} else if (s.failure.field) {
			await focus(s.failure.field, true);
		}
	}
</script>

<div bind:this={root} class="flex flex-col gap-6">
	<ol aria-label={m.auth_setup_steps()} class="flex flex-wrap gap-x-4 gap-y-1 text-small">
		{#each steps as s, i (s.id)}
			<li
				aria-current={s.id === step ? 'step' : undefined}
				class={s.id === step ? 'font-semibold text-base-content' : 'text-muted-text'}
			>
				{i + 1}. {s.label()}
			</li>
		{/each}
	</ol>

	{#if step === 'token'}
		<form method="post" class="flex flex-col gap-4" novalidate onsubmit={continueToNotice}>
			<h2 class="text-title font-semibold text-base-content">{m.auth_setup_step_token()}</h2>
			<Field
				label={m.auth_token_label()}
				help={m.auth_token_help({ format: TOKEN_HINT })}
				error={errorFor('token')}
				required
			>
				{#snippet children(props)}
					<Input
						{...props}
						bind:value={token}
						mono
						name="token"
						autocomplete="off"
						autocapitalize="characters"
						spellcheck="false"
						placeholder={TOKEN_HINT}
						oninput={() => form.edit()}
					/>
				{/snippet}
			</Field>
			<details class="rounded-box border border-base-300 p-3 text-small text-base-content">
				<summary class="cursor-pointer font-medium">{m.auth_token_where()}</summary>
				<ul class="mt-2 flex list-disc flex-col gap-1 pl-5">
					<li>
						{m.auth_token_where_docker()} <code class="font-mono">docker logs lodestar</code>
					</li>
					<li>{m.auth_token_where_unraid()}</li>
					<li>
						{m.auth_token_where_windows()}
						<code class="font-mono">%ProgramData%\Lodestar\logs</code>
					</li>
					<li>
						{m.auth_token_where_linux()} <code class="font-mono">journalctl -u lodestar</code>
					</li>
				</ul>
			</details>
			<FormAlert {form} />
			<Button type="submit" variant="primary">{m.auth_token_next()}</Button>
		</form>
	{:else if step === 'notice'}
		<form method="post" class="flex flex-col gap-4" novalidate onsubmit={continueToPassword}>
			<h2 class="text-title font-semibold text-base-content">{m.auth_setup_step_notice()}</h2>
			<p class="text-body text-muted-text">{m.auth_notice_intro()}</p>
			<NoticeBox bind:notice />
			{#if notice}
				<div class="flex flex-col gap-1">
					<Checkbox
						bind:checked={read}
						name="notice"
						label={m.auth_notice_confirm()}
						required
						aria-invalid={noticeError ? true : undefined}
						aria-describedby={noticeError ? 'notice-error' : undefined}
						onchange={() => form.edit()}
					/>
					{#if noticeError}
						<p
							id="notice-error"
							class="flex items-start gap-1 text-small text-base-content"
							data-testid="field-error"
						>
							<IconWarning class="mt-0.5 size-4 shrink-0 text-error" aria-hidden="true" />
							<span>{noticeError}</span>
						</p>
					{/if}
				</div>
			{/if}
			<FormAlert {form} />
			<div class="flex flex-col-reverse gap-3 medium:flex-row medium:justify-between">
				<Button variant="quiet" onclick={back}>{m.auth_back()}</Button>
				<Button type="submit" variant="primary" aria-disabled={!notice || undefined}
					>{m.auth_token_next()}</Button
				>
			</div>
		</form>
	{:else}
		<form method="post" class="flex flex-col gap-4" novalidate onsubmit={submit}>
			<h2 class="text-title font-semibold text-base-content">{m.auth_setup_step_password()}</h2>
			<!-- For password managers: the instance has one account, so the name is fixed. -->
			<input type="text" name="username" autocomplete="username" value="admin" hidden readonly />
			<Field
				label={m.auth_new_password_label()}
				help={[m.auth_new_password_help({ min: String(MIN_PASSWORD_CHARS) }), strengthHint]
					.filter(Boolean)
					.join(' ')}
				error={errorFor('password')}
				required
			>
				{#snippet children(props)}
					<Input
						{...props}
						bind:value={password}
						type="password"
						name="password"
						autocomplete="new-password"
						oninput={() => form.edit()}
					/>
				{/snippet}
			</Field>
			<Field label={m.auth_confirm_label()} error={errorFor('confirm')} required>
				{#snippet children(props)}
					<Input
						{...props}
						bind:value={confirm}
						type="password"
						name="confirm"
						autocomplete="new-password"
						oninput={() => form.edit()}
					/>
				{/snippet}
			</Field>
			<FormAlert {form} />
			<div class="flex flex-col-reverse gap-3 medium:flex-row medium:justify-between">
				<Button variant="quiet" onclick={back} disabled={form.busy}>{m.auth_back()}</Button>
				<Button
					type="submit"
					variant="primary"
					loading={form.busy}
					aria-disabled={form.busy || locked || undefined}
				>
					{m.auth_setup_submit()}
				</Button>
			</div>
		</form>
	{/if}
</div>
