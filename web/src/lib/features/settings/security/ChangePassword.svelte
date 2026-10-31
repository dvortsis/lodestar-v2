<script lang="ts">
	// Change password (§10.10.7: it needs the current one, so no second confirmation). The server's
	// rules show under the new password before anything is sent: at least 10 characters, at most
	// 1,024 bytes, and the strength hint, which only advises. The server ends every other session
	// on success (`POST /password`), and the result says so. All three fields are cleared once
	// the request goes, whatever comes back. While anything is typed, leaving asks first; the
	// passwords never reach the engine's draft, so nothing survives a reload.
	import { ApiError, api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import {
		checkPassword,
		isBlocking,
		MAX_PASSWORD_BYTES,
		MIN_PASSWORD_CHARS
	} from '$lib/features/auth/password';
	import { refusal } from './confirm';
	import { guardLeave } from './guard';
	import Result from './Result.svelte';

	let current = $state('');
	let next = $state('');
	let again = $state('');
	let busy = $state(false);
	let errors = $state<{ old?: string; new?: string; confirm?: string }>({});
	let done = $state('');
	let failed = $state('');
	/** The latest lock-out (429); a new object restarts the countdown. */
	let lockout = $state<{ seconds: number; message: string }>();
	let locked = $state(false);
	let unlock: ReturnType<typeof setTimeout> | undefined;
	/** The lock-out banner, loaded with the first 429; `false` when it couldn't load. */
	let Lockout = $state<typeof import('./Lockout.svelte').default | false>();

	function lock(seconds: number, message: string) {
		lockout = { seconds, message };
		locked = true;
		clearTimeout(unlock);
		unlock = setTimeout(() => (locked = false), seconds * 1000);
		if (!Lockout)
			import('./Lockout.svelte').then(
				(l) => (Lockout = l.default),
				() => (Lockout = false)
			);
	}
	$effect(() => () => clearTimeout(unlock));
	let formEl = $state<HTMLFormElement>();

	const strength = $derived(next ? checkPassword(next) : null);
	const rules = $derived(
		[
			m.auth_new_password_help({ min: String(MIN_PASSWORD_CHARS) }),
			strength === 'weak'
				? m.auth_strength_weak()
				: strength === 'fair'
					? m.auth_strength_fair()
					: strength === 'strong'
						? m.auth_strength_strong()
						: ''
		]
			.filter(Boolean)
			.join(' ')
	);
	const typed = $derived(current !== '' || next !== '' || again !== '');

	const focus = (name: string) =>
		formEl?.querySelector<HTMLInputElement>(`input[name="${name}"]`)?.focus();

	function clear() {
		current = next = again = '';
	}

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		if (busy || locked) return;
		done = failed = '';
		const check = checkPassword(next);
		errors = {
			old: current === '' ? m.auth_error_required() : undefined,
			new: isBlocking(check)
				? check === 'too_long'
					? m.auth_error_too_long({ max: String(MAX_PASSWORD_BYTES) })
					: m.auth_error_too_short({ min: String(MIN_PASSWORD_CHARS) })
				: undefined,
			confirm: again !== next ? m.auth_error_mismatch() : undefined
		};
		const first = (['old', 'new', 'confirm'] as const).find((k) => errors[k]);
		if (first)
			return focus(
				first === 'old' ? 'current-password' : first === 'new' ? 'new-password' : 'confirm-password'
			);
		const body = { old: current, new: next };
		clear();
		busy = true;
		try {
			await api.post('/password', { body });
			done = m.settings_security_password_done();
			lockout = undefined;
		} catch (err) {
			const r = refusal(err, m.settings_security_failed());
			if (r.signedOut) return;
			const field = err instanceof ApiError ? err.field : undefined;
			// A lock-out isn't any field's fault: the banner says it.
			if (r.lockedFor !== undefined) lock(r.lockedFor, r.message);
			else if (field === 'old' || field === 'new') errors = { [field]: r.message };
			else failed = r.message;
			focus(field === 'new' ? 'new-password' : 'current-password');
		} finally {
			busy = false;
		}
	}

	// Leaving with something typed asks first (§10.10.5), through the screen's one guard.
	guardLeave({
		dirty: () => typed,
		clear,
		section: () => m.settings_security_password_heading()
	});
</script>

<form
	bind:this={formEl}
	method="post"
	novalidate
	class="flex flex-col gap-4"
	aria-labelledby="security-password"
	onsubmit={submit}
	data-testid="security-password"
>
	<h2 id="security-password" class="text-label font-semibold">
		{m.settings_security_password_heading()}
	</h2>
	<p class="text-muted-text">{m.settings_security_password_signs_out()}</p>
	<!-- For password managers: the instance has one account, so the name is fixed. -->
	<input type="text" name="username" autocomplete="username" value="admin" hidden readonly />
	<Field label={m.settings_security_password_current()} error={errors.old} required>
		{#snippet children(props)}
			<Input
				{...props}
				bind:value={current}
				type="password"
				name="current-password"
				autocomplete="current-password"
				readonly={busy || undefined}
			/>
		{/snippet}
	</Field>
	<Field label={m.settings_security_password_new()} help={rules} error={errors.new} required>
		{#snippet children(props)}
			<Input
				{...props}
				bind:value={next}
				type="password"
				name="new-password"
				autocomplete="new-password"
				readonly={busy || undefined}
			/>
		{/snippet}
	</Field>
	<Field label={m.settings_security_password_confirm()} error={errors.confirm} required>
		{#snippet children(props)}
			<Input
				{...props}
				bind:value={again}
				type="password"
				name="confirm-password"
				autocomplete="new-password"
				readonly={busy || undefined}
			/>
		{/snippet}
	</Field>
	{#if lockout && Lockout}
		{#key lockout}
			<Lockout seconds={lockout.seconds} />
		{/key}
	{:else if lockout && Lockout === false}
		<p role="alert">{lockout.message}</p>
	{/if}
	<div class="flex flex-wrap items-center gap-3">
		<Button
			type="submit"
			variant="primary"
			loading={busy}
			aria-disabled={busy || locked || undefined}
			data-testid="security-password-submit">{m.settings_security_password_submit()}</Button
		>
		<Result
			text={failed ? `${m.settings_security_password_failed()} ${failed}` : done}
			failed={!!failed}
			testid="security-password-result"
		/>
	</div>
</form>
