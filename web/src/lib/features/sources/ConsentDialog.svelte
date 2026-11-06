<script lang="ts">
	// The External, Restricted and Overlay consent dialogs (§18.7, owner §2.49): the approved text,
	// verbatim, with only what the server asks for beside it: nothing (External) or the admin password
	// (Restricted, Overlay). Submitting sends the enable the server's own row describes; the answer
	// decides. A refusal, the X-consent-gate-052 one included, is shown here in plain words.
	// The Account dialog (credential fields) is SRC-15c's.
	import { untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { blocksFor, enableRefusal, needsPassword, richParts, type ConsentClass } from './consent';
	import type { Source } from './sources';

	interface Props {
		open?: boolean;
		source: Source;
		/** Sends `PATCH /sources/{id}` with this body; resolves when the server took it. */
		onenable: (body: {
			consent: ConsentClass;
			consent_version: string;
			password?: string;
		}) => Promise<void>;
	}
	let { open = $bindable(false), source: s, onenable }: Props = $props();

	const cls = $derived(s.consent as ConsentClass);
	const blocks = $derived(blocksFor(s, cls));
	const asksPassword = $derived(needsPassword(cls));
	const uid = $props.id();
	const cancelId = `${uid}-cancel`;
	const formId = `${uid}-form`;

	let password = $state('');
	let passwordError = $state('');
	let problem = $state('');
	let busy = $state(false);

	function reset() {
		password = '';
		passwordError = '';
		problem = '';
	}
	// The password never outlives the dialog: opening and closing both start from an empty field.
	$effect(() => {
		if (open) untrack(reset);
	});
	$effect(() => {
		if (!open) untrack(reset);
	});

	async function submit(e: Event) {
		e.preventDefault();
		if (busy) return;
		if (asksPassword && password === '') {
			passwordError = m.sources_cx_password_needed();
			return;
		}
		passwordError = '';
		problem = '';
		busy = true;
		try {
			await onenable({
				consent: cls,
				consent_version: s.consent_version,
				...(asksPassword ? { password } : {})
			});
			open = false;
		} catch (err) {
			const r = enableRefusal(err);
			problem = r.text;
			if (r.password) password = '';
		} finally {
			busy = false;
		}
	}
</script>

{#snippet rich(text: string)}
	{#each richParts(text) as part, i (i)}{#if part.bold}<strong>{part.text}</strong
			>{:else}{part.text}{/if}{/each}
{/snippet}

<Dialog
	bind:open
	title={blocks.title}
	closeLabel={m.sources_cx_close()}
	initialFocus={asksPassword ? () => document.getElementById(cancelId) : undefined}
>
	<form id={formId} class="flex flex-col gap-4" onsubmit={submit} autocomplete="off" novalidate>
		<div class="flex flex-col gap-3" data-testid="consent-text">
			{#each blocks.paragraphs as p, i (i)}
				<p data-testid="consent-p">{@render rich(p)}</p>
			{/each}
			{#if blocks.bullets.length > 0}
				<ul class="ms-5 flex list-disc flex-col gap-1">
					{#each blocks.bullets as b, i (i)}
						<li data-testid="consent-b">{@render rich(b)}</li>
					{/each}
				</ul>
			{/if}
			{#if blocks.closing}<p data-testid="consent-p">{@render rich(blocks.closing)}</p>{/if}
		</div>
		{#if asksPassword}
			<Field
				label={m.sources_cx_password()}
				help={m.sources_cx_password_help()}
				error={passwordError}
				required
			>
				{#snippet children(c)}
					<Input
						{...c}
						type="password"
						bind:value={password}
						autocomplete="current-password"
						data-testid="consent-password"
					/>
				{/snippet}
			</Field>
		{/if}
		{#if problem}
			<div data-testid="consent-problem"><Banner kind="error">{problem}</Banner></div>
		{/if}
	</form>
	{#snippet footer()}
		<Button id={cancelId} onclick={() => (open = false)} data-testid="consent-cancel">
			{m.sources_cx_cancel()}
		</Button>
		<Button
			variant="primary"
			type="submit"
			form={formId}
			loading={busy}
			data-testid="consent-confirm"
		>
			{blocks.confirm}
		</Button>
	{/snippet}
</Dialog>
