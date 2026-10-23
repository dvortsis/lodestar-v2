<script lang="ts">
	// The whole field of §10.10.4: label with the modified marker and the field menu, the control
	// the registry picks (or Automatic), help, the error, the status line, an inline preview, a
	// conflict, and what the last save or the engine did. A screen never builds a field by hand.
	import Button from '$lib/ui/controls/Button.svelte';
	import IconButton from '$lib/ui/controls/IconButton.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import Menu, { type MenuItem } from '$lib/ui/overlays/Menu.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconRestart from '~icons/ph/arrows-clockwise';
	import IconCheck from '~icons/ph/check-circle';
	import IconMenu from '~icons/ph/dots-three';
	import IconFile from '~icons/ph/file';
	import IconInfo from '~icons/ph/info';
	import IconLock from '~icons/ph/lock-simple';
	import IconWarning from '~icons/ph/warning';
	import Control from './Control.svelte';
	import SecretControl from './SecretControl.svelte';
	import { automaticNumber, automaticValue, displayValue, estimatePart } from './display';
	import type { SettingsForm } from './form.svelte';
	import { fieldHelp, fieldLabel } from './messages';
	import { fieldAnchor } from './schema';
	import type { FieldKey } from './types';
	import { widgetFor } from './widgets';

	interface Props {
		form: SettingsForm;
		key: FieldKey;
		dense?: boolean;
	}

	let { form, key, dense = false }: Props = $props();

	const f = $derived(form.field(key));
	const spec = $derived(f.spec);
	const label = $derived(fieldLabel(spec));
	const help = $derived(fieldHelp(spec));
	const choice = $derived(widgetFor(spec));
	// A toggle carries its own label (the `Toggle` primitive); the header keeps marker and menu.
	const ownLabel = $derived('builtin' in choice && choice.builtin === 'toggle');
	const uid = $props.id();
	const labelId = `${uid}-label`;

	// Automatic (`x-auto`): on stages the sentinel; off shows the control. `manual` keeps the
	// control open while its value happens to equal the sentinel (an empty text, a typed 0).
	const hasAuto = $derived(spec.auto !== undefined && !spec.writeOnly);
	let manual = $state(false);
	const automatic = $derived(hasAuto && !manual && f.value === spec.auto);
	const effective = $derived(automaticValue(f));

	function setAutomatic(on: boolean) {
		manual = !on;
		if (on) form.set(key, spec.auto);
		else {
			const n = automaticNumber(f);
			// Pre-filled with the effective value; empty (and invalid) when there's none.
			form.set(key, n ?? (typeof spec.auto === 'number' ? '' : f.value));
		}
	}

	const readOnly = $derived(f.locked && f.env === null);
	const menu = $derived<MenuItem[]>([
		...(f.source === 'db' && !f.locked && !spec.writeOnly
			? [{ id: 'reset', label: m.settings_form_menu_reset() }]
			: []),
		{ id: 'link', label: m.settings_form_menu_copy_link() },
		...(spec.config || spec.env ? [{ id: 'keys', label: m.settings_form_menu_show_keys() }] : [])
	]);
	let showKeys = $state(false);

	async function copy(text: string, done: string) {
		try {
			await navigator.clipboard.writeText(text);
			toast.push({ kind: 'success', text: done, timeout: 3000 });
		} catch {
			toast.push({ kind: 'error', text: m.settings_form_copy_failed() });
		}
	}

	function onMenu(id: string) {
		if (id === 'reset') form.reset(key);
		else if (id === 'link') {
			const href = form.href(key);
			void copy(new URL(href, location.href).href, m.settings_form_link_copied());
		} else if (id === 'keys') showKeys = !showKeys;
	}

	const estimates = $derived(
		(f.preview?.estimates ?? []).flatMap((e) => {
			const part = estimatePart(e);
			return part ? [part] : [];
		})
	);
	// With Automatic on, its one estimate is shown beside the switch instead.
	const previewParts = $derived(automatic && effective ? [] : estimates);
	const warnings = $derived(f.preview?.warnings ?? []);
</script>

<div id={fieldAnchor(key)} class="ls-field flex flex-col gap-2" data-field={key}>
	<Field {help} error={f.error}>
		{#snippet children(cp)}
			<div class="flex items-center justify-between gap-2">
				{#if ownLabel}
					<span>
						{#if f.changed}
							<span class="ls-marker" role="img" aria-label={m.setting_changed()}></span>
						{/if}
					</span>
				{:else}
					<label id={labelId} for={cp.id} class="text-small font-medium text-base-content">
						{label}
						{#if f.changed}
							<span class="ls-marker" role="img" aria-label={m.setting_changed()}></span>
						{/if}
					</label>
				{/if}
				<Menu items={menu} onSelect={onMenu} label={m.settings_form_menu_label({ label })}>
					{#snippet trigger({ props })}
						<IconButton {...props} dense label={m.settings_form_menu_label({ label })}>
							<IconMenu />
						</IconButton>
					{/snippet}
				</Menu>
			</div>
			{@const controlProps = { ...cp, 'aria-labelledby': ownLabel ? undefined : labelId }}
			{#if hasAuto}
				<div class="flex flex-wrap items-center gap-3">
					<Toggle
						label={m.setting_automatic()}
						checked={automatic}
						disabled={f.locked}
						onchange={(e) => setAutomatic(e.currentTarget.checked)}
					/>
					{#if automatic && effective}
						<span class="text-small text-muted-text" data-testid="automatic-value">
							{m.settings_form_automatic_value({ value: effective })}
						</span>
					{/if}
				</div>
			{/if}
			{#if !automatic}
				{#if 'component' in choice}
					{@const Widget = choice.component}
					<Widget {form} {key} {controlProps} {dense} />
				{:else if choice.builtin === 'secret'}
					<SecretControl {form} {key} {controlProps} {label} {dense} />
				{:else}
					<Control
						{form}
						{key}
						{controlProps}
						{label}
						{dense}
						widget={choice.builtin}
						mono={choice.mono}
					/>
				{/if}
			{/if}
		{/snippet}
	</Field>

	{#if previewParts.length || warnings.length}
		<p class="flex flex-wrap items-center gap-x-4 gap-y-1 text-small text-muted-text">
			{#each previewParts as p (p.testid)}
				<span data-testid={p.testid}>
					{p.text}
					{#if p.mono}<Mono>{p.mono}</Mono>{/if}
				</span>
			{/each}
			{#each warnings as w (w.code)}
				<span class="inline-flex items-center gap-1 text-warning">
					<IconWarning class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
					<span class="text-base-content">{w.message}</span>
				</span>
			{/each}
		</p>
	{/if}

	{#if f.conflict}
		<div class="flex flex-wrap items-center gap-2 text-small" data-testid="conflict">
			<IconWarning class="size-(--icon-size-row) shrink-0 text-warning" aria-hidden="true" />
			<span>{m.settings_form_conflict({ value: displayValue(spec, f.conflict.theirs) })}</span>
			<Button dense onclick={() => form.resolve(key, 'mine')}>{m.settings_form_keep_mine()}</Button>
			<Button dense onclick={() => form.resolve(key, 'theirs')}
				>{m.settings_form_use_theirs()}</Button
			>
		</div>
	{/if}

	{#if f.note}
		<p class="flex items-center gap-1 text-small">
			<IconInfo class="size-(--icon-size-row) shrink-0 text-info" aria-hidden="true" />
			{f.note}
		</p>
	{/if}

	{#if f.pendingReset}
		<p class="flex items-center gap-1 text-small text-muted-text">
			<IconRestart class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
			{m.settings_form_pending_reset()}
		</p>
	{/if}

	<!-- Status line: only what applies (§10.10.4). -->
	<div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-small text-muted-text">
		{#if f.savedRecently}
			<span class="inline-flex items-center gap-1 text-success">
				<IconCheck class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				<span class="text-base-content">
					{f.savedRecently === 'restart'
						? m.settings_form_saved_restart()
						: m.settings_form_saved()}
				</span>
			</span>
		{:else if spec.restartRequired && !f.locked}
			<span class="inline-flex items-center gap-1">
				<IconRestart class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				{m.setting_restart()}
			</span>
		{/if}
		{#if f.env}
			<span class="inline-flex flex-wrap items-center gap-1" data-testid="status-env">
				<IconLock class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				<span class="text-base-content">{m.setting_env({ variable: f.env })}</span>
				<Button
					dense
					variant="quiet"
					onclick={() => copy(f.env!, m.settings_form_copied())}
					aria-label={m.settings_form_copy_variable({ variable: f.env })}
					>{m.settings_form_copy()}</Button
				>
				<span>{m.settings_form_env_help()}</span>
			</span>
		{:else if readOnly}
			<span class="inline-flex flex-wrap items-center gap-1" data-testid="status-read-only">
				<IconLock class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				<span class="text-base-content">{m.settings_form_read_only()}</span>
				{#if typeof f.value === 'string' || typeof f.value === 'number'}
					<Button
						dense
						variant="quiet"
						onclick={() => copy(String(f.value), m.settings_form_copied())}
						aria-label={m.settings_form_copy_value({ label })}>{m.settings_form_copy()}</Button
					>
				{/if}
			</span>
		{:else if f.source === 'file'}
			<span class="inline-flex items-center gap-1" data-testid="status-file">
				<IconFile class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				{m.setting_file()}
			</span>
		{/if}
		{#if f.undoUntil}
			<span class="inline-flex items-center gap-1">
				<IconCheck class="size-(--icon-size-row) shrink-0 text-success" aria-hidden="true" />
				<span class="text-base-content">{m.settings_form_announce_changed()}</span>
				<Button dense variant="quiet" onclick={() => form.undo(key)}
					>{m.settings_form_undo()}</Button
				>
			</span>
		{/if}
	</div>

	{#if showKeys}
		<dl class="grid grid-cols-2 gap-x-4 gap-y-1 text-small" data-testid="field-keys">
			{#if spec.config}
				<dt class="text-muted-text">{m.settings_form_config_key()}</dt>
				<dd><Mono>{spec.config}</Mono></dd>
			{/if}
			{#if spec.env}
				<dt class="text-muted-text">{m.settings_form_env_var()}</dt>
				<dd><Mono>{spec.env}</Mono></dd>
			{/if}
		</dl>
	{/if}
</div>

<style>
	.ls-field {
		/* A focused field scrolls clear of the save bar (§10.10.5). */
		scroll-margin-bottom: calc(var(--spacing) * 24);
		scroll-margin-top: calc(var(--spacing) * 4);
	}
	.ls-marker {
		display: inline-block;
		vertical-align: middle;
		margin-inline-start: calc(var(--spacing) * 1);
		width: calc(var(--spacing) * 2);
		height: calc(var(--spacing) * 2);
		border-radius: 50%;
		background: var(--color-primary);
	}
</style>
