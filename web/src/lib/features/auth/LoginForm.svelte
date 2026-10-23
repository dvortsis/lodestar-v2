<script lang="ts">
	// Sign in with the admin password, then go back to `next` (already validated by safeNext).
	import { onDestroy, tick } from 'svelte';
	import { api } from '$lib/api/client';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { failureMessage } from './errors';
	import { createAuthForm } from './form.svelte';
	import FormAlert from './FormAlert.svelte';
	import { toNext, toSetup } from './nav';
	import { setSession } from './session.svelte';

	interface Props {
		next: string;
	}

	let { next }: Props = $props();

	const form = createAuthForm();
	onDestroy(form.destroy);

	let password = $state('');
	let formEl = $state<HTMLFormElement>();

	const fieldError = $derived(
		form.state.name === 'error' && form.state.failure.field === 'password'
			? failureMessage(form.state.failure)
			: undefined
	);
	const locked = $derived(form.state.name === 'locked');

	async function focusField() {
		await tick();
		const input = formEl?.querySelector<HTMLInputElement>('input[name="password"]');
		input?.focus();
		input?.select();
	}

	async function onsubmit(event: SubmitEvent) {
		event.preventDefault();
		if (form.busy || locked) return;
		if (!password) {
			form.invalid({ kind: 'invalid', field: 'password', reason: 'required' });
			return focusField();
		}
		const ok = await form.submit(() => api.post('/login', { body: { password } }));
		if (ok) {
			password = '';
			setSession('signed_in');
			await toNext(next);
			return;
		}
		const s = form.state;
		if (s.name === 'error' && s.failure.kind === 'not_set_up') {
			setSession('setup_required');
			await toSetup();
		} else if (s.name === 'error' && s.failure.field) {
			await focusField();
		}
	}
</script>

<form bind:this={formEl} method="post" class="flex flex-col gap-4" novalidate {onsubmit}>
	<!-- For password managers: the instance has one account, so the name is fixed. -->
	<input type="text" name="username" autocomplete="username" value="admin" hidden readonly />
	<Field label={m.auth_password_label()} error={fieldError} required>
		{#snippet children(props)}
			<Input
				{...props}
				bind:value={password}
				type="password"
				name="password"
				autocomplete="current-password"
				oninput={() => form.edit()}
			/>
		{/snippet}
	</Field>
	<FormAlert {form} />
	<Button
		type="submit"
		variant="primary"
		loading={form.busy}
		aria-disabled={form.busy || locked || undefined}
	>
		{m.auth_login_submit()}
	</Button>
</form>
