<script lang="ts">
	// "Save search" / "New alert" (§10.8): a name, the query and the channels the API has, with
	// visible labels. Loaded with `import()` on first open so Bits UI's overlay code stays out of the
	// route's first chunk. S-search-3 mounts it from the results toolbar (README.md).
	import { goto } from '$app/navigation';
	import { untrack } from 'svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { alertPath, createSavedSearch, type SavedSearch } from './api';
	import { saveFailure, type SaveFailure } from './errors';

	interface Props {
		open?: boolean;
		/** Prefills the query (Search passes the current one). */
		q?: string;
		/** Passed through to the server when given (Search's sort); the Alerts page sends none. */
		sort?: string | null;
		/** Decides what happens next; the dialog then only toasts. Default: open the new alert. */
		onSaved?: (saved: SavedSearch) => void;
	}
	let { open = $bindable(false), q = '', sort = null, onSaved }: Props = $props();

	const formId = $props.id();
	let name = $state('');
	let query = $state('');
	let alertOn = $state(true);
	let rss = $state(false);
	let submitting = $state(false);
	let failure = $state<SaveFailure>({});
	let form = $state<HTMLFormElement>();

	// Each opening starts clean: the defaults, the prefilled query and no old error.
	$effect(() => {
		if (!open) return;
		untrack(() => {
			name = '';
			query = q;
			alertOn = true;
			rss = false;
			failure = {};
			submitting = false;
		});
	});

	const nameError = $derived(failure.field === 'name' ? failure.message : undefined);
	const queryError = $derived(
		failure.field === 'q'
			? failure.detail
				? `${failure.message} ${failure.detail}`
				: failure.message
			: undefined
	);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (submitting) return;
		const trimmed = name.trim();
		if (trimmed === '' || [...trimmed].length > 200) {
			failure = { field: 'name', message: m.alerts_err_name() };
			form?.querySelector<HTMLElement>('[name="name"]')?.focus();
			return;
		}
		submitting = true;
		failure = {};
		try {
			const saved = await createSavedSearch({
				name: trimmed,
				q: query.trim() === '' ? undefined : query,
				sort: sort ?? undefined,
				alert: alertOn,
				rss
			});
			open = false;
			toast.push({ kind: 'success', text: m.alerts_saved({ name: saved.name }) });
			if (onSaved) onSaved(saved);
			// eslint-disable-next-line svelte/no-navigation-without-resolve -- alertPath() resolves
			else void goto(alertPath(saved.id));
		} catch (e) {
			failure = saveFailure(e, query, currentLocale());
			const input = failure.field
				? form?.querySelector<HTMLInputElement>(`[name="${failure.field}"]`)
				: null;
			if (input) {
				input.focus();
				if (failure.select) input.setSelectionRange(...failure.select);
			}
		} finally {
			submitting = false;
		}
	}
</script>

<!-- The dialog opens on its first field, so the name can be typed at once. -->
<Dialog
	bind:open
	title={m.alerts_create_title()}
	closeLabel={m.alerts_close()}
	initialFocus={() => form?.querySelector<HTMLElement>('[name="name"]')}
>
	<form id={formId} bind:this={form} class="al-form" onsubmit={submit} novalidate>
		{#if failure.form}
			<Banner kind="error">{failure.form}</Banner>
		{/if}
		<Field label={m.alerts_field_name()} required error={nameError}>
			{#snippet children(control)}
				<Input {...control} name="name" bind:value={name} autocomplete="off" />
			{/snippet}
		</Field>
		<Field label={m.alerts_field_query()} help={m.alerts_query_help()} error={queryError}>
			{#snippet children(control)}
				<Input
					{...control}
					name="q"
					mono
					bind:value={query}
					autocomplete="off"
					autocapitalize="off"
					spellcheck="false"
				/>
			{/snippet}
		</Field>
		<fieldset class="al-fieldset">
			<legend class="al-legend">{m.alerts_notifications()}</legend>
			<Checkbox label={m.alerts_notify_app()} bind:checked={alertOn} name="alert" />
			<Checkbox label={m.alerts_notify_rss()} bind:checked={rss} name="rss" />
		</fieldset>
	</form>
	{#snippet footer()}
		<Button onclick={() => (open = false)}>{m.alerts_cancel()}</Button>
		<Button type="submit" form={formId} variant="primary" loading={submitting}
			>{m.alerts_save()}</Button
		>
	{/snippet}
</Dialog>

<style>
	.al-form {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
	}
	.al-fieldset {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
		border: 0;
		margin: 0;
		padding: 0;
	}
	.al-legend {
		margin-bottom: calc(var(--spacing) * 1);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-weight: var(--font-weight-medium);
	}
</style>
