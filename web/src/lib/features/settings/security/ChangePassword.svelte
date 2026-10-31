<script lang="ts">
	// Change password (§10.10.7: it needs the current one, so no second confirmation). The server's
	// rules show under the new password before anything is sent: at least 10 characters, at most
	// 1,024 bytes, and the strength hint, which only advises. The server ends every other session
	// on success (`POST /password`), and the result says so. All three fields are cleared once
	// the request goes, whatever comes back. While anything is typed, leaving asks first; the
	// passwords never reach the engine's draft, so nothing survives a reload.
	import { beforeNavigate, goto } from '$app/navigation';
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
	import { ask } from '../form/dialogs';
	import { refusal } from './confirm';

	let current = $state('');
	let next = $state('');
	let again = $state('');
	let busy = $state(false);
	let errors = $state<{ old?: string; new?: string; confirm?: string }>({});
	let done = $state('');
	let failed = $state('');
	let lockout = $state<{ seconds: number }>();
	let locked = $state(false);
	/** The lock-out banner, loaded with the first 429. */
	let Lockout = $state<typeof import('./Lockout.svelte').default>();
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
		} catch (err) {
			const r = refusal(err, m.settings_security_failed());
			if (r.signedOut) return;
			const field = err instanceof ApiError ? err.field : undefined;
			if (field === 'old' || field === 'new') errors = { [field]: r.message };
			else failed = r.message;
			if (r.lockedFor !== undefined) {
				lockout = { seconds: r.lockedFor };
				locked = true;
				void import('./Lockout.svelte').then((l) => (Lockout = l.default));
			}
			focus(field === 'new' ? 'new-password' : 'current-password');
		} finally {
			busy = false;
		}
	}

	// Leaving with something typed asks first, as the engine's leave guard decides (§10.10.5,
	// `form/leave.ts`, restated here: importing it would split it out of the engine's chunk);
	// closing or reloading the tab lets the browser ask; a hash change on this page isn't leaving.
	let bypass = false;
	beforeNavigate((nav) => {
		if (!typed || bypass) return;
		const from = nav.from?.url;
		const to = nav.to?.url;
		if (
			nav.type !== 'leave' &&
			from &&
			to &&
			from.origin === to.origin &&
			from.pathname === to.pathname
		)
			return;
		nav.cancel();
		if (nav.type === 'leave' || !to) return;
		void ask({
			title: m.settings_security_leave_title(),
			body: [m.settings_security_leave_body()],
			actions: [
				{ id: 'stay', label: m.settings_security_leave_stay() },
				{ id: 'leave', label: m.settings_security_leave_discard(), variant: 'danger' }
			],
			initial: 'stay'
		}).then(async (answer) => {
			if (answer !== 'leave') return;
			clear();
			if (to.origin !== location.origin) return location.assign(to.href);
			bypass = true;
			try {
				// eslint-disable-next-line svelte/no-navigation-without-resolve -- the router's own target, already resolved
				await goto(to.href);
			} finally {
				bypass = false;
			}
		});
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
			<Lockout seconds={lockout.seconds} onchange={(l) => (locked = l)} />
		{/key}
	{/if}
	<div class="flex flex-wrap items-center gap-3">
		<Button
			type="submit"
			variant="primary"
			loading={busy}
			aria-disabled={locked || undefined}
			data-testid="security-password-submit">{m.settings_security_password_submit()}</Button
		>
		<p
			role="status"
			class="text-small {failed ? 'text-error' : 'text-success'}"
			data-testid="security-password-result"
		>
			{failed ? `${m.settings_security_password_failed()} ${failed}` : done}
		</p>
	</div>
</form>
