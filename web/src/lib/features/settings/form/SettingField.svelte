<script lang="ts">
	// The whole field of §10.10.4: label with the modified marker and the field menu, the control
	// the registry picks (or Automatic), help, the error, the status line, an inline preview, a
	// conflict, and what the last save or the engine did. A screen never builds a field by hand.
	import Button from '$lib/ui/controls/Button.svelte';
	import IconButton from '$lib/ui/controls/IconButton.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconRestart from '~icons/ph/arrows-clockwise';
	import IconMenu from '~icons/ph/dots-three';
	import IconFile from '~icons/ph/file';
	import IconInfo from '~icons/ph/info';
	import IconLock from '~icons/ph/lock-simple';
	import IconWarning from '~icons/ph/warning';
	import Control from './Control.svelte';
	import LazyControl from './LazyControl.svelte';
	import { controlOf, lazyOf } from './controls.svelte';
	import { lazyPart } from './lazy.svelte';
	import FieldConflict from './FieldConflict.svelte';
	import FieldSaved from './FieldSaved.svelte';
	import { loadedPreview, type SettingsForm } from './form.svelte';
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
	// A `<label for>` only where one control takes the id: radio groups, segmented controls, a
	// secret and registered widgets are labelled by the label's id instead.
	// A control that loads on demand takes the id only once it's in: until then nothing does.
	const loaded = $derived.by(() => {
		const file = 'builtin' in choice ? lazyOf(choice.builtin) : undefined;
		return !file || controlOf(file) !== undefined;
	});
	const singleControl = $derived(
		automatic ||
			(loaded &&
				'builtin' in choice &&
				(['number', 'slider', 'text', 'select', 'json'].includes(choice.builtin) ||
					// A locked chip list has no input to label.
					(choice.builtin === 'chips' && !f.locked)))
	);
	// Discard, Undo or Use theirs bringing the sentinel back turns Automatic on again.
	$effect(() => {
		const v = f.value;
		const clean = !f.dirty;
		untrack(() => {
			if (manual && clean && v === spec.auto) manual = false;
		});
	});

	function setAutomatic(on: boolean) {
		manual = !on;
		if (on) form.set(key, spec.auto);
		else {
			const n = Preview.part?.automaticNumber(f);
			// Pre-filled with the effective value; empty (and invalid) when there's none.
			form.set(key, n ?? (typeof spec.auto === 'number' ? '' : f.value));
		}
	}

	const readOnly = $derived(f.locked && f.env === null);
	let showKeys = $state(false);

	// The menu loads when it's first opened; until then a plain button stands in for its trigger.
	let menuParts = $state<typeof import('./menu')>();
	let menuOpen = $state(false);
	let menuFailed = $state(false);
	async function openMenu() {
		try {
			menuParts ??= await import('./menu');
		} catch {
			menuFailed = true; // browsers keep a failed module: say so, once
			return;
		}
		menuOpen = true;
	}

	function copy(text: string, what: 'value' | 'link') {
		let done: Promise<void>;
		try {
			done = navigator.clipboard.writeText(text); // in the click: Safari allows it only there
		} catch (e) {
			done = Promise.reject(e); // no clipboard (an insecure origin)
		}
		done.catch(() => {}); // reported by the toast
		void import('./copy').then(
			(c) => c.toastCopy(done, what),
			() => {}
		); // no toast offline
	}

	function onMenu(id: string) {
		if (id === 'reset') form.reset(key);
		else if (id === 'link') {
			const href = form.href(key);
			void copy(new URL(href, location.href).href, 'link');
		} else if (id === 'keys') showKeys = !showKeys;
	}

	// What a preview says loads with the preview's code, which every preview has loaded.
	const Preview = lazyPart(() => import('./preview'), loadedPreview);
	$effect(() => Preview.want(f.preview));
</script>

<div id={fieldAnchor(key)} class="ls-field flex flex-col gap-2" data-field={key}>
	<Field {help} error={f.error}>
		{#snippet children(cp)}
			<div class="flex items-center justify-between gap-2">
				<span class="flex items-center gap-1">
					{#if ownLabel}
						<!-- The Toggle below carries the label. -->
					{:else if singleControl}
						<label id={labelId} for={cp.id} class="text-small font-medium text-base-content"
							>{label}</label
						>
					{:else}
						<span id={labelId} class="text-small font-medium text-base-content">{label}</span>
					{/if}
					{#if f.changed}
						<span class="ls-marker" role="img" aria-label={m.setting_changed()}></span>
					{/if}
				</span>
				{#if menuParts}
					<menuParts.FieldMenu
						field={f}
						onSelect={onMenu}
						label={m.settings_form_menu_label({ label })}
						bind:open={menuOpen}
					/>
				{:else}
					<IconButton
						dense
						label={m.settings_form_menu_label({ label })}
						aria-haspopup="menu"
						onclick={openMenu}
					>
						<IconMenu />
					</IconButton>
				{/if}
			</div>
			{@const controlProps = { ...cp, 'aria-labelledby': ownLabel ? undefined : labelId }}
			<div class="flex flex-col gap-2" data-control>
				{#if hasAuto}
					<div class="flex flex-wrap items-center gap-3">
						<!-- While on, the switch is the field's control: help and errors are tied to it. -->
						<Toggle
							{...automatic ? cp : {}}
							label={m.setting_automatic()}
							checked={automatic}
							disabled={f.locked}
							onchange={(e) => setAutomatic(e.currentTarget.checked)}
						/>
						{#if automatic && f.preview && Preview.part}
							<Preview.part.FieldPreview field={f} automatic part="automatic" />
						{/if}
					</div>
				{/if}
				{#if !automatic}
					{#if 'component' in choice}
						{@const Widget = choice.component}
						<Widget {form} {key} {controlProps} {dense} />
					{:else if choice.builtin === 'secret'}
						<LazyControl builtin="secret" {form} {key} {controlProps} {label} {dense} />
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
			</div>
		{/snippet}
	</Field>

	{#if f.preview && Preview.part}
		<Preview.part.FieldPreview field={f} {automatic} part="lines" />
	{:else if f.previewUnavailable || menuFailed}
		<p class="text-small text-error">{m.settings_form_chunk_failed()}</p>
	{/if}

	{#if f.conflict}
		<div class="flex flex-wrap items-center gap-2 text-small" data-testid="conflict">
			<IconWarning class="size-(--icon-size-row) shrink-0 text-warning" aria-hidden="true" />
			<FieldConflict {form} {key} />
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
			<FieldSaved {form} {key} part="reset" />
		</p>
	{/if}

	<!-- Status line: only what applies (§10.10.4). -->
	<div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-small text-muted-text">
		{#if f.savedRecently}
			<FieldSaved {form} {key} part="saved" />
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
					onclick={() => copy(f.env!, 'value')}
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
						onclick={() => copy(String(f.value), 'value')}
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
			<FieldSaved {form} {key} part="undo" />
		{/if}
	</div>

	{#if showKeys && menuParts}
		<menuParts.FieldKeys {spec} />
	{/if}
</div>

<style>
	.ls-field {
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
