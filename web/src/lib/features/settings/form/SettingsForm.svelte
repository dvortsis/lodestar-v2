<script lang="ts">
	// Everything a form needs that's tied to a component's lifetime (§10.10.5): the error summary,
	// the draft and network banners, the one polite live region, the save bar, ⌘/Ctrl+S, the leave
	// guard, the `settings` subscription and the default disruptive confirmation. The screen puts
	// its groups and `SettingField`s in `children`.
	import { beforeNavigate, goto } from '$app/navigation';
	import { events } from '$lib/state/events.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { tick, type Snippet } from 'svelte';
	import IconRestart from '~icons/ph/arrows-clockwise';
	import IconCheck from '~icons/ph/check-circle';
	import { ask } from './dialogs';
	import type { SettingsForm } from './form.svelte';
	import { hardNavigate, leaveDecision } from './leave';
	import { fieldLabel } from './messages';
	import type { FieldKey, GateContext } from './types';

	interface Props {
		form: SettingsForm;
		/** The pinned save bar; off for the wizard, which saves on Next. */
		saveBar?: boolean;
		/** Ask before leaving with unsaved changes; off for the wizard. */
		leaveGuard?: boolean;
		/** ⌘/Ctrl+S saves. */
		shortcut?: boolean;
		/** Called before focusing a field from the error summary (expand Advanced, say). */
		onReveal?: (key: FieldKey) => void | Promise<void>;
		children: Snippet;
	}

	let {
		form,
		saveBar = true,
		leaveGuard = true,
		shortcut = true,
		onReveal,
		children
	}: Props = $props();

	let summary = $state<HTMLElement>();
	let summaryShown = $state(false);
	let bypass = false;

	const unsaved = $derived(form.dirtyCount > 0);
	const barShown = $derived(
		saveBar &&
			(unsaved ||
				form.status === 'saving' ||
				form.status === 'saved' ||
				form.status === 'save-error' ||
				form.status === 'offline' ||
				form.undoUntil !== null)
	);
	const count = $derived(formatCount(form.dirtyCount, currentLocale()));
	const retrySeconds = $derived(form.saveError?.retryAfter);

	// --- saving ------------------------------------------------------------------------------

	/** Every save path goes here: the bar, ⌘/Ctrl+S, the leave dialog. */
	async function save() {
		const r = await form.save();
		if (!r.ok && form.errors.length) {
			summaryShown = true;
			await tick();
			summary?.focus();
		} else if (r.ok) summaryShown = false;
		return r;
	}

	async function discard() {
		// One change is cheap to redo; more than one asks first.
		if (form.dirtyCount > 1) {
			const answer = await ask({
				title: m.settings_form_discard_title({ count }),
				body: [m.settings_form_discard_body()],
				actions: [
					{ id: 'keep', label: m.settings_form_keep_editing() },
					{ id: 'discard', label: m.settings_form_discard(), variant: 'danger' }
				],
				initial: 'keep'
			});
			if (answer !== 'discard') return;
		}
		form.discard();
		summaryShown = false;
	}

	const CONTROL_SELECTORS = [
		'input:checked',
		'[role="radio"][aria-checked="true"]',
		'input, select, textarea, [role="radio"], button'
	];

	async function reveal(key: FieldKey) {
		await onReveal?.(key);
		await tick();
		// The field's control, never its header (the ⋯ menu): the label's target, else the checked
		// option, else the first control inside the field's control area.
		const root = document.getElementById(key);
		const area = root?.querySelector<HTMLElement>('[data-control]');
		const label = root?.querySelector<HTMLLabelElement>('label[for]');
		const control =
			(label?.htmlFor ? document.getElementById(label.htmlFor) : null) ??
			// In order of preference (a selector list would match in document order instead).
			CONTROL_SELECTORS.map((q) => area?.querySelector<HTMLElement>(q)).find(Boolean);
		root?.scrollIntoView?.({ block: 'center' });
		control?.focus();
	}

	// --- the default disruptive confirmation (§10.10.7) --------------------------------------

	function effectLines(c: GateContext): string[] {
		const lines: string[] = [];
		for (const key of c.fields) {
			const [section, name] = [key.slice(0, key.indexOf('.')), key.slice(key.indexOf('.') + 1)];
			const p = c.preview.sections[section]?.fields[name];
			for (const e of p?.estimates ?? []) {
				if (e.name === 'url' && typeof e.value === 'string') {
					lines.push(m.settings_form_disruptive_url({ url: e.value }));
				}
			}
			for (const w of p?.warnings ?? []) lines.push(w.message);
		}
		return [...new Set(lines)];
	}

	// A fallback: a screen's own disruptive handler (Network's lockout dialog, §10.10.6) wins.
	$effect(() =>
		form.defaultGate('disruptive', async (c) => {
			const labels = c.fields
				.map((k) => form.specs.find((s) => s.key === k))
				.filter((s) => s !== undefined)
				.map((s) => fieldLabel(s));
			const effects = effectLines(c);
			const answer = await ask({
				title: m.settings_form_disruptive_title(),
				body: [
					m.settings_form_disruptive_fields({ fields: labels.join(', ') }),
					...(effects.length ? effects : [m.settings_form_disruptive_generic()])
				],
				actions: [
					{ id: 'cancel', label: m.settings_form_cancel() },
					{ id: 'save', label: m.settings_form_save(), variant: 'primary' }
				],
				initial: 'cancel'
			});
			return answer === 'save';
		})
	);

	// --- events ------------------------------------------------------------------------------

	$effect(() =>
		events.on('settings', (e) => form.received(e), { onResync: () => void form.resync() })
	);

	// --- ⌘/Ctrl+S ----------------------------------------------------------------------------

	const isSave = (e: KeyboardEvent) =>
		(e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 's';

	/**
	 * Inside the form, handled here before the registry (which listens on `window`) sees it, so
	 * a press never fires twice; the registry covers the rest of the page once it has loaded.
	 */
	function onkeydown(e: KeyboardEvent) {
		if (!shortcut || e.defaultPrevented || !isSave(e)) return;
		// A held key repeats: the browser's own "Save page" must not open on the repeats either.
		e.preventDefault();
		if (!e.repeat) void save();
	}

	// --- the bar never covers the focused field (§10.10.5) -----------------------------------

	let bar = $state<HTMLElement>();
	function onfocusin(e: FocusEvent) {
		if (!bar || !(e.target instanceof HTMLElement) || bar.contains(e.target)) return;
		const target = e.target.getBoundingClientRect();
		const top = bar.getBoundingClientRect().top;
		// The ramp's 4 (16 px at the default root size) of air between the control and the bar.
		const air = parseFloat(getComputedStyle(document.documentElement).fontSize);
		const hidden = target.bottom + air - top;
		if (hidden > 0) window.scrollBy({ top: hidden });
	}

	$effect(() => {
		if (!shortcut) return;
		let off: (() => void) | undefined;
		let gone = false;
		// Loaded on demand, as the shell does (W9): the registry never enters a static import.
		void import('$lib/keys').then(({ keys }) => {
			if (gone) return;
			const scope = `settings-form:${form.id}`;
			const pop = keys.pushScope(scope);
			let unregister: (() => void) | undefined;
			try {
				unregister = keys.register({
					id: `settings.save.${form.id}`,
					title: () => m.settings_form_save(),
					keys: ['mod+s'],
					scope,
					allowInInput: true,
					run: () => void save()
				});
			} catch {
				// Another form on the page registered first; this one still saves from its own keys.
			}
			off = () => {
				unregister?.();
				pop();
			};
		});
		return () => {
			gone = true;
			off?.();
		};
	});

	// --- the leave guard ---------------------------------------------------------------------

	beforeNavigate((nav) => {
		if (!leaveGuard) return;
		const decision = leaveDecision({
			dirty: form.dirtyCount > 0,
			type: nav.type,
			from: nav.from?.url ?? null,
			to: nav.to?.url ?? null,
			bypass
		});
		if (decision === 'allow') return;
		nav.cancel(); // for 'leave', this raises the browser's own prompt
		if (decision === 'prompt' && nav.to) void confirmLeave(nav.to.url, nav.willUnload);
	});

	/** Asks Save / Discard / Keep editing, then continues to `to` the way the user was going. */
	async function confirmLeave(to: URL, unload: boolean) {
		const answer = await ask({
			title: m.settings_form_leave_title(),
			body: [m.settings_form_leave_body({ count })],
			actions: [
				{ id: 'keep', label: m.settings_form_keep_editing() },
				{ id: 'discard', label: m.settings_form_discard() },
				{ id: 'save', label: m.settings_form_save(), variant: 'primary' }
			],
			initial: 'save'
		});
		if (answer === 'save') {
			// Nothing to save any more (a merge or ⌘S settled it meanwhile) is as good as saved.
			const r = await save();
			if (!r.ok && r.reason !== 'nothing') return;
		} else if (answer === 'discard') form.discard();
		else return;
		// Another site, or a link that reloads the page: the router can't take it there. Nothing is
		// unsaved by now, so the unload passes the guard without a bypass.
		if (unload || to.origin !== location.origin) {
			hardNavigate(to.href);
			return;
		}
		bypass = true;
		try {
			// eslint-disable-next-line svelte/no-navigation-without-resolve -- the router's own target, already resolved
			await goto(to.href);
		} finally {
			bypass = false;
		}
	}
</script>

<div
	class="ls-form flex flex-col gap-6"
	class:ls-form-bar={barShown}
	role="presentation"
	{onkeydown}
	{onfocusin}
>
	{#if summaryShown && form.errors.length}
		<div class="rounded-box border border-error p-4" data-testid="error-summary">
			<h2 bind:this={summary} tabindex="-1" class="text-title font-semibold">
				{m.settings_form_summary_title()}
			</h2>
			<ul class="mt-2 flex flex-col gap-1">
				{#each form.errors as e, i (e.key ?? `form-${i}`)}
					<li>
						{#if e.key}
							{@const key = e.key}
							{@const spec = form.specs.find((s) => s.key === key)}
							<a
								class="link inline-flex items-center text-error pointer-coarse:min-h-(--target-min)"
								href="#{key}"
								onclick={(ev) => {
									ev.preventDefault();
									void reveal(key);
								}}>{spec ? `${fieldLabel(spec)}: ` : ''}{e.message}</a
							>
						{:else}
							<span class="text-error">{e.message}</span>
						{/if}
					</li>
				{/each}
			</ul>
		</div>
	{/if}

	{#if form.draft}
		<Banner kind="info" title={m.settings_form_draft_title()}>
			<span data-testid="draft-text"
				>{form.draft.dropped
					? m.settings_form_draft_dropped({
							count: formatCount(form.draft.dropped, currentLocale())
						})
					: m.settings_form_draft_body()}</span
			>
			{#snippet action()}
				<div class="flex gap-2">
					<Button dense onclick={() => form.dropDraft()}>{m.settings_form_discard()}</Button>
					<Button dense variant="primary" onclick={() => form.restoreDraft()}
						>{m.settings_form_restore()}</Button
					>
				</div>
			{/snippet}
		</Banner>
	{/if}

	{#if form.status === 'offline'}
		<Banner kind="warning">
			<span data-testid="offline-text"
				>{retrySeconds
					? m.settings_form_busy({ seconds: formatCount(retrySeconds, currentLocale()) })
					: m.settings_form_offline()}</span
			>
			{#snippet action()}
				<Button dense onclick={() => void form.retry()}>{m.settings_form_retry()}</Button>
			{/snippet}
		</Banner>
	{:else if form.status === 'save-error'}
		<Banner kind="error">
			{m.settings_form_error_server()}
			{#snippet action()}
				<Button dense onclick={() => void form.retry()}>{m.settings_form_retry()}</Button>
			{/snippet}
		</Banner>
	{/if}

	{#if form.status === 'loading'}
		<p class="text-muted-text" aria-busy="true">{m.settings_form_loading()}</p>
	{:else if form.status === 'load-error'}
		<Banner kind="error">
			{m.settings_form_load_error()}
			{#snippet action()}
				<Button dense onclick={() => void form.retry()}>{m.settings_form_retry()}</Button>
			{/snippet}
		</Banner>
	{:else}
		{@render children()}
	{/if}

	<!-- One polite live region: saves, conflicts, "Updated elsewhere" (§10.10.11). -->
	<div class="sr-only" aria-live="polite" aria-atomic="true">
		{#key form.announcement?.id}{form.announcement?.text ?? ''}{/key}
	</div>

	{#if barShown}
		<div
			class="ls-bar sticky bottom-0 z-10 flex flex-wrap items-center justify-between gap-3 border-t border-base-300 bg-base-100 py-2"
			data-testid="save-bar"
			bind:this={bar}
		>
			<p class="flex flex-wrap items-center gap-2 text-body" data-testid="save-bar-status">
				{#if form.status === 'saving'}
					{m.settings_form_saving()}
				{:else if unsaved}
					{form.dirtyCount === 1
						? m.settings_form_unsaved_one()
						: m.settings_form_unsaved({ count })}
				{:else if form.status === 'saved' || form.undoUntil !== null}
					<span class="inline-flex items-center gap-1">
						{#if form.restartPending.length}
							<IconRestart class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
						{:else}
							<IconCheck class="size-(--icon-size-row) shrink-0 text-success" aria-hidden="true" />
						{/if}
						{form.restartPending.length ? m.settings_form_saved_restart() : m.settings_form_saved()}
					</span>
					{#if form.undoSkipsSecrets}
						<span class="text-small text-muted-text">{m.settings_form_undo_secrets()}</span>
					{/if}
				{/if}
			</p>
			<div class="flex items-center gap-2">
				{#if unsaved}
					<Button onclick={discard}>{m.settings_form_discard()}</Button>
					<!-- Never disabled: with errors, Save shows the summary (§10.10.5). -->
					<Button variant="primary" loading={form.status === 'saving'} onclick={() => void save()}
						>{m.settings_form_save()}</Button
					>
				{:else if form.undoUntil !== null}
					<Button onclick={() => void form.undo()}>{m.settings_form_undo()}</Button>
				{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	/* While the bar shows, the column keeps room for it under the last field. */
	.ls-form-bar {
		padding-bottom: calc(var(--spacing) * 4);
	}
	@media (pointer: coarse) {
		.ls-bar {
			min-height: var(--target-min);
		}
	}
</style>
