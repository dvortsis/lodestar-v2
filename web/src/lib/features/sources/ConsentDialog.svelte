<script lang="ts">
	// The External, Restricted and Overlay consent dialogs (§18.7, owner §2.49): the approved text,
	// verbatim, with only what the server asks for beside it: nothing (External) or the admin password
	// (Restricted, Overlay). Submitting sends the enable the server's own row describes; the answer
	// decides. A refusal, the X-consent-gate-052 one included, is shown here in plain words.
	// The Account dialog (SRC-15c) adds the credential fields from `credential_spec`: secrets are
	// write-only, never prefilled or read back, sent once to `PATCH /sources/{id}/credentials` and
	// cleared from the component whether that worked or not. Enabling is then asked of the server,
	// whose answer is shown as it is (the gate stays closed in v0.5.2: no client-side bypass).
	import { tick, untrack } from 'svelte';
	import { api, ApiError } from '$lib/api/client';
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
			consent?: ConsentClass;
			consent_version?: string;
			password?: string;
		}) => Promise<void>;
		/** Credentials were stored or removed: reload the list so the row shows the server's names. */
		onsaved?: () => void;
		/** The notice changed since the list was read: reload it so the next try sends the new version. */
		onstale: () => void;
	}
	let { open = $bindable(false), source: s, onenable, onstale, onsaved }: Props = $props();

	const SEPARATOR = ', ';
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
	const isAccount = $derived(cls === 'account');
	const spec = $derived(s.credential_spec);
	// Typed values by credential name. Never logged, stored or shown; emptied after every submit.
	let creds = $state<Record<string, string>>({});
	let credErrors = $state<Record<string, string>>({});
	let saved = $state(false);
	// The credential whose Remove awaits confirmation (removing is destructive and can't be undone).
	let removing = $state<string | null>(null);
	// A changed notice: Confirm stays off until the dialog is reopened, so the new list is read first.
	let stale = $state(false);

	function reset() {
		password = '';
		passwordError = '';
		problem = '';
		stale = false;
		creds = {};
		credErrors = {};
		saved = false;
		removing = null;
	}
	// The password never outlives the dialog: opening and closing both start from an empty field.
	$effect(() => {
		if (open) untrack(reset);
	});
	$effect(() => {
		if (!open) untrack(reset);
	});

	const storedHas = (name: string) => s.credentials.includes(name);

	/** Stores what was typed, then asks the server to turn the source on; shows its answer. */
	async function submitAccount() {
		credErrors = {};
		for (const f of spec)
			if (f.required && !storedHas(f.name) && (creds[f.name] ?? '') === '')
				credErrors[f.name] = m.sources_cx_credential_needed();
		if (Object.keys(credErrors).length > 0) {
			problem = m.sources_cx_credential_fix();
			const first = Object.keys(credErrors)[0]!;
			(
				document.querySelector(`[data-testid="credential-${first}"]`) as HTMLElement | null
			)?.focus();
			return;
		}
		problem = '';
		saved = false;
		busy = true;
		const typed = Object.fromEntries(Object.entries(creds).filter(([, v]) => v !== ''));
		try {
			if (Object.keys(typed).length > 0) {
				try {
					await api.patch('/sources/{id}/credentials', {
						path: { id: s.id },
						body: { credentials: typed }
					});
				} finally {
					creds = {};
				}
				saved = true;
				onsaved?.();
			}
			await onenable({});
			open = false;
		} catch (err) {
			// After a save the refusal is about turning on; "Nothing was changed" would contradict it.
			const r = enableRefusal(err);
			problem = r.text.replace(/ Nothing was changed\.$/, '');
			if (r.stale) {
				stale = true;
				onstale();
			}
		} finally {
			creds = {};
			busy = false;
		}
	}

	const cancelRemoveId = `${uid}-keep`;
	async function askRemove(name: string) {
		removing = name;
		await tick();
		document.getElementById(cancelRemoveId)?.focus();
	}

	async function remove(name: string) {
		if (busy) return;
		removing = null;
		busy = true;
		problem = '';
		try {
			await api.patch('/sources/{id}/credentials', {
				path: { id: s.id },
				body: { credentials: { [name]: null } }
			});
			onsaved?.();
		} catch (err) {
			problem =
				err instanceof ApiError && err.status !== 0 && err.message
					? m.sources_cx_gate({ reason: err.message })
					: m.sources_error_network();
		} finally {
			busy = false;
		}
	}

	async function submit(e: Event) {
		e.preventDefault();
		if (busy) return;
		if (isAccount) return submitAccount();
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
			if (r.stale) {
				stale = true;
				onstale();
			}
		} finally {
			busy = false;
		}
	}
</script>

{#snippet addresses()}
	{#if s.endpoints.length === 0}
		{m.sources_addresses_none().replace(/\.$/, '')}
	{:else}
		{#each s.endpoints as address, k (k)}{#if k > 0}{SEPARATOR}{/if}<code
				class="font-mono break-all">{address}</code
			>{/each}
	{/if}
{/snippet}

{#snippet pieces(list: string[])}
	{#each list as piece, k (k)}{#if k > 0}{@render addresses()}{/if}{piece}{/each}
{/snippet}

{#snippet rich(text: string)}
	{#each richParts(text) as part, i (i)}{#if part.bold}<strong>{@render pieces(part.pieces)}</strong
			>{:else}{@render pieces(part.pieces)}{/if}{/each}
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
		{#if isAccount}
			<div class="flex flex-col gap-3" data-testid="credential-fields">
				{#each spec as f (f.name)}
					<Field
						label={f.label}
						help={storedHas(f.name) ? m.sources_cx_credential_stored() : undefined}
						error={credErrors[f.name]}
						required={f.required && !storedHas(f.name)}
					>
						{#snippet children(c)}
							<div class="flex items-start gap-2">
								<Input
									{...c}
									type={f.kind === 'secret' ? 'password' : 'text'}
									bind:value={creds[f.name]}
									autocomplete={f.kind === 'secret' ? 'new-password' : 'off'}
									spellcheck={false}
									data-testid="credential-{f.name}"
								/>
								{#if storedHas(f.name) && removing === f.name}
									<Button
										id={cancelRemoveId}
										dense
										onclick={() => (removing = null)}
										data-testid="credential-{f.name}-keep"
									>
										{m.sources_cx_credential_keep()}
									</Button>
									<Button
										dense
										variant="danger"
										onclick={() => remove(f.name)}
										data-testid="credential-{f.name}-remove-yes"
									>
										{m.sources_cx_credential_remove_yes()}<span class="sr-only"> {f.label}</span>
									</Button>
								{:else if storedHas(f.name)}
									<Button
										dense
										onclick={() => askRemove(f.name)}
										data-testid="credential-{f.name}-remove"
									>
										{m.sources_cx_credential_remove()}<span class="sr-only"> {f.label}</span>
									</Button>
								{/if}
							</div>
						{/snippet}
					</Field>
				{/each}
			</div>
			{#if saved}<Banner kind="info">{m.sources_cx_credentials_saved()}</Banner>{/if}
		{/if}
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
			disabled={stale}
			data-testid="consent-confirm"
		>
			{blocks.confirm}
		</Button>
	{/snippet}
</Dialog>
