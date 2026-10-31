<script lang="ts">
	// The password confirmation (§10.10.7, B12), opened through `confirmWithPassword` (confirm.ts).
	// Focus starts in the password field. The password lives only in this field: it's cleared
	// before the request goes, whatever comes back. A wrong password shows the server's own words;
	// the lock-out (429) shows how long to wait and holds the button until then. Cancel sends
	// nothing. A closed session closes the dialog, and the API client takes the user to sign in.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import type { Outcome } from './confirm';

	interface Props {
		title: string;
		body: string[];
		action: string;
		send: (password: string) => Promise<Outcome>;
		/** Once: `{ value }` after a change went through, null when closed without one. */
		onclose: (result: { value: unknown } | null) => void;
	}
	let { title, body, action, send, onclose }: Props = $props();

	const uid = $props.id();
	const formId = `${uid}-form`;
	let open = $state(true);
	let password = $state('');
	let busy = $state(false);
	let error = $state('');
	/** The latest lock-out (429); a new object restarts the countdown. */
	let lockout = $state<{ seconds: number }>();
	let locked = $state(false);
	/** The lock-out banner, loaded with the first 429. */
	let Lockout = $state<typeof import('./Lockout.svelte').default>();
	let closed = false;

	function close(result: { value: unknown } | null) {
		if (closed) return;
		closed = true;
		password = '';
		open = false;
		onclose(result);
	}

	const field = () => document.querySelector<HTMLInputElement>(`#${formId} input[name="password"]`);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		if (busy || locked) return;
		const typed = password;
		password = '';
		if (typed === '') {
			error = m.settings_security_confirm_empty();
			field()?.focus();
			return;
		}
		busy = true;
		error = '';
		const outcome = await send(typed);
		busy = false;
		if ('value' in outcome) return close(outcome);
		const r = outcome.refused;
		if (r.signedOut) return close(null);
		error = r.message;
		if (r.lockedFor !== undefined) {
			lockout = { seconds: r.lockedFor };
			locked = true;
			void import('./Lockout.svelte').then((l) => (Lockout = l.default));
		}
		field()?.focus();
	}
</script>

<Dialog
	bind:open
	{title}
	closeLabel={m.settings_form_dialog_close()}
	initialFocus={field}
	onOpenChange={(o) => {
		// A close asked for while the change is on its way waits for the answer.
		if (!o && busy) open = true;
		else if (!o) close(null);
	}}
>
	<form id={formId} method="post" novalidate class="flex flex-col gap-4" onsubmit={submit}>
		{#each body as p, i (i)}
			<p data-testid="password-dialog-body-{i}">{p}</p>
		{/each}
		<!-- For password managers: the instance has one account, so the name is fixed. -->
		<input type="text" name="username" autocomplete="username" value="admin" hidden readonly />
		<Field
			label={m.settings_security_confirm_label()}
			help={m.settings_security_confirm_help()}
			error={error || undefined}
			required
		>
			{#snippet children(props)}
				<Input
					{...props}
					bind:value={password}
					type="password"
					name="password"
					autocomplete="current-password"
					readonly={busy || undefined}
					data-testid="password-dialog-input"
				/>
			{/snippet}
		</Field>
		{#if lockout && Lockout}
			{#key lockout}
				<Lockout seconds={lockout.seconds} onchange={(l) => (locked = l)} />
			{/key}
		{/if}
	</form>
	{#snippet footer()}
		<Button onclick={() => close(null)} disabled={busy} data-testid="password-dialog-cancel"
			>{m.settings_form_cancel()}</Button
		>
		<Button
			type="submit"
			form={formId}
			variant="primary"
			loading={busy}
			aria-disabled={locked || undefined}
			data-testid="password-dialog-confirm">{action}</Button
		>
	{/snippet}
</Dialog>
