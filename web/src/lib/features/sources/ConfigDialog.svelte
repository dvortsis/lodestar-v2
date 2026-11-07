<script lang="ts">
	// A source's settings (SRC-15c, §18.8), a form generated from its `config_schema`. The API doesn't
	// return saved values, so the form starts from each setting's default and a save replaces the
	// settings, which the text says. The checks save a round trip; the server judges and its
	// sentence is shown. Settings the form can't draw are named, never guessed at.
	import { untrack } from 'svelte';
	import { ApiError } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import {
		blankRow,
		cellKey,
		initialValues,
		isList,
		parseSchema,
		toConfig,
		validate,
		type Errors,
		type ListField,
		type Row,
		type ScalarField
	} from './config-form';
	import type { Source } from './sources';

	interface Props {
		open?: boolean;
		source: Source;
		/** Sends `PATCH /sources/{id}` with this `config`; resolves when the server took it. */
		onsave: (config: Record<string, unknown>) => Promise<void>;
	}
	let { open = $bindable(false), source: s, onsave }: Props = $props();

	const fields = $derived(parseSchema(s.config_schema) ?? []);
	const drawn = $derived(fields.filter((f) => f.kind !== 'unsupported'));
	const skipped = $derived(fields.filter((f) => f.kind === 'unsupported'));
	const uid = $props.id();
	const cancelId = `${uid}-cancel`;
	const formId = `${uid}-form`;

	let scalars = $state<Record<string, string | boolean>>({});
	let lists = $state<Record<string, (string | Row)[]>>({});
	let errors = $state<Errors>({});
	let problem = $state('');
	let busy = $state(false);

	function reset() {
		const v = initialValues(fields);
		scalars = {};
		lists = {};
		for (const [k, x] of Object.entries(v))
			if (Array.isArray(x)) lists[k] = [...x];
			else scalars[k] = x;
		errors = {};
		problem = '';
	}
	$effect(() => {
		if (open) untrack(reset);
	});

	async function submit(e: Event) {
		e.preventDefault();
		if (busy) return;
		const values = { ...scalars, ...lists } as Parameters<typeof validate>[1];
		errors = validate(fields, values);
		if (Object.keys(errors).length > 0) {
			problem = m.sources_cfg_fix();
			return;
		}
		problem = '';
		busy = true;
		try {
			await onsave(toConfig(fields, values));
			open = false;
		} catch (err) {
			if (!(err instanceof ApiError) || err.status === 0) problem = m.sources_error_network();
			else
				problem = err.message
					? m.sources_cx_gate({ reason: err.message })
					: m.sources_error_code({ code: err.code });
		} finally {
			busy = false;
		}
	}
</script>

{#snippet control(f: ScalarField, o: Record<string, string | boolean>, err: string, row: number)}
	{#if f.kind === 'boolean'}
		<Checkbox
			label={f.label}
			checked={o[f.name] === true}
			onchange={(e) => (o[f.name] = e.currentTarget.checked)}
		/>
	{:else}
		<Field
			label={row ? m.sources_cfg_item({ label: f.label, n: row }) : f.label}
			help={f.help}
			error={err}
			required={f.required}
		>
			{#snippet children(c)}
				{#if f.kind === 'enum'}
					<Select
						{...c}
						options={f.options.map((v) => ({ value: v, label: v }))}
						placeholder={f.required ? undefined : ''}
						bind:value={o[f.name] as string}
					/>
				{:else}
					<Input
						{...c}
						bind:value={o[f.name] as string}
						type="text"
						inputmode={f.kind === 'integer'
							? 'numeric'
							: f.kind === 'number'
								? 'decimal'
								: undefined}
					/>
				{/if}
			{/snippet}
		</Field>
	{/if}
{/snippet}

{#snippet list(f: ListField)}
	{@const rows = lists[f.name] ?? []}
	<fieldset class="flex flex-col gap-2" data-testid="cfg-{f.name}">
		<legend class="text-small font-medium">{f.label}</legend>
		{#if f.help}<p class="text-small text-muted-text">{f.help}</p>{/if}
		{#each rows as row, i (i)}
			<div class="flex flex-wrap items-start gap-2" data-testid="cfg-{f.name}-row">
				<div class="flex min-w-0 flex-1 flex-col gap-2">
					{#if typeof row === 'string'}
						<Field error={errors[cellKey(f.name, i)]}>
							{#snippet children(c)}
								<Input
									{...c}
									bind:value={rows[i] as string}
									aria-label={m.sources_cfg_item({ label: f.label, n: i + 1 })}
								/>
							{/snippet}
						</Field>
					{:else}
						{#each f.item as sub (sub.name)}
							{@render control(sub, row, errors[cellKey(f.name, i, sub.name)] ?? '', i + 1)}
						{/each}
						{#if errors[cellKey(f.name, i)]}<p class="text-small text-error">
								{errors[cellKey(f.name, i)]}
							</p>{/if}
					{/if}
				</div>
				<Button dense onclick={() => (lists[f.name] = rows.filter((_, k) => k !== i))}>
					{m.sources_cfg_remove()}<span class="sr-only"> {f.label} {i + 1}</span>
				</Button>
			</div>
		{/each}
		{#if errors[f.name]}<p class="text-small text-error" role="alert">{errors[f.name]}</p>{/if}
		<div>
			<Button
				dense
				onclick={() => (lists[f.name] = [...rows, f.kind === 'strings' ? '' : blankRow(f)])}
				disabled={f.maxItems !== undefined && rows.length >= f.maxItems}
				data-testid="cfg-{f.name}-add"
			>
				{m.sources_cfg_add()}<span class="sr-only"> {f.label}</span>
			</Button>
		</div>
	</fieldset>
{/snippet}

<Dialog
	bind:open
	title={m.sources_cfg_title({ name: s.name })}
	closeLabel={m.sources_cx_close()}
	initialFocus={() => document.getElementById(cancelId)}
>
	<form id={formId} class="flex flex-col gap-4" onsubmit={submit} autocomplete="off" novalidate>
		<p data-testid="cfg-intro">{m.sources_cfg_intro()}</p>
		{#each drawn as f (f.name)}
			{#if isList(f)}
				{@render list(f)}
			{:else}
				{@render control(f, scalars, errors[f.name] ?? '', 0)}
			{/if}
		{/each}
		{#each skipped as f (f.name)}
			<p class="text-small text-muted-text" data-testid="cfg-unsupported">
				{m.sources_cfg_unsupported({ label: f.label })}
			</p>
		{/each}
		{#if fields.length === 0}
			<p class="text-muted-text" data-testid="cfg-unsupported">{m.sources_cfg_none()}</p>
		{/if}
		{#if problem}<div data-testid="cfg-problem"><Banner kind="error">{problem}</Banner></div>{/if}
	</form>
	{#snippet footer()}
		<Button id={cancelId} onclick={() => (open = false)} data-testid="cfg-cancel">
			{m.sources_cx_cancel()}
		</Button>
		<Button
			variant="primary"
			type="submit"
			form={formId}
			loading={busy}
			disabled={drawn.length === 0}
			data-testid="cfg-save"
		>
			{m.sources_cfg_save()}
		</Button>
	{/snippet}
</Dialog>
