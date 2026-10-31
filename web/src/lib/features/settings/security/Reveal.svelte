<script lang="ts">
	// A new key's secret, shown this once with Copy (B12). It lives only in this dialog's props:
	// closing it unmounts the dialog, and nothing else ever held it.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import Result from './Result.svelte';

	let { name, secret, onclose }: { name: string; secret: string; onclose: () => void } = $props();

	let open = $state(true);
	let copied = $state<'' | 'done' | 'failed'>('');
	const input = () => document.querySelector<HTMLInputElement>('[data-testid="key-secret"]');

	function close() {
		open = false;
		onclose();
	}

	function copy() {
		// The write runs in the click, where browsers allow it.
		navigator.clipboard.writeText(secret).then(
			() => (copied = 'done'),
			() => {
				copied = 'failed';
				input()?.select();
			}
		);
	}
</script>

<Dialog
	bind:open
	title={m.settings_security_reveal_title({ name })}
	closeLabel={m.settings_form_dialog_close()}
	initialFocus={input}
	onOpenChange={(o) => {
		if (!o) close();
	}}
>
	<div class="flex flex-col gap-4">
		<p>{m.settings_security_reveal_body()}</p>
		<Field label={m.settings_security_reveal_label()}>
			{#snippet children(props)}
				<Input
					{...props}
					value={secret}
					mono
					readonly
					autocomplete="off"
					spellcheck="false"
					onfocus={(e) => e.currentTarget.select()}
					data-testid="key-secret"
				/>
			{/snippet}
		</Field>
		<div class="flex flex-wrap items-center gap-3">
			<Button onclick={copy} data-testid="key-secret-copy"
				>{m.settings_security_reveal_copy()}</Button
			>
			<Result
				text={copied === 'done'
					? m.settings_security_reveal_copied()
					: copied === 'failed'
						? m.settings_security_reveal_copy_failed()
						: ''}
				failed={copied === 'failed'}
				testid="key-secret-copied"
			/>
		</div>
	</div>
	{#snippet footer()}
		<Button variant="primary" onclick={close} data-testid="key-secret-done"
			>{m.settings_security_reveal_done()}</Button
		>
	{/snippet}
</Dialog>
