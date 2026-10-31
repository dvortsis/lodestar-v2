<script lang="ts">
	// Everything a form needs that's tied to a component's lifetime (§10.10.5): the error summary,
	// the draft and network banners, the one polite live region, the save bar, ⌘/Ctrl+S, the leave
	// guard, the `settings` subscription and the default disruptive confirmation. The screen puts
	// its groups and `SettingField`s in `children`. The bar, the summary, the banners and the
	// questions are in the edit chunk, which the form loads with the first edit.
	import { beforeNavigate, goto } from '$app/navigation';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { onDestroy, tick, untrack, type Snippet } from 'svelte';
	import { loadControls } from './controls.svelte';
	import { lazyPart } from './lazy.svelte';
	import { loadEdit, loadedEdit, preloadSettingsForm, type SettingsForm } from './form.svelte';
	import { hardNavigate, leaveDecision } from './leave';
	import { loadMessages, messagesLoaded } from './messages';
	import type { FieldKey } from './types';
	import { widgetFor } from './widgets';

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

	// The labels and help of these fields' sections: nothing renders before they're in, so a raw
	// key never flashes. Already loaded (a return visit) renders at once.
	const sections = untrack(() => form.specs.map((spec) => spec.section));
	const inPlace = messagesLoaded(sections);
	let labelsReady = $state(inPlace);
	if (!inPlace) void loadMessages(sections).then(() => (labelsReady = true));

	let summaryShown = $state(false);
	let summaryFocus = $state(0);
	let summaryFocused = $state(0);
	let focusSeq = 0; // per component: a new `form` starts its saves at 1 again
	let bypass = false;

	const barShown = $derived(
		saveBar &&
			(form.dirtyCount > 0 ||
				form.status === 'saving' ||
				form.status === 'saved' ||
				form.status === 'save-error' ||
				form.status === 'offline' ||
				form.undoUntil !== null)
	);

	const notice = $derived(form.status === 'offline' || form.status === 'save-error');

	// From the edit chunk. `peek` renders a part in the same update as what shows it once the
	// chunk is in: an offered draft with the fields (the form waits for it), a save's result.
	const BarPart = lazyPart(
		() => loadEdit().then((mod) => mod.SaveBar),
		() => loadedEdit()?.SaveBar
	);
	const SummaryPart = lazyPart(
		() => loadEdit().then((mod) => mod.ErrorSummary),
		() => loadedEdit()?.ErrorSummary
	);
	const NoticePart = lazyPart(
		() => loadEdit().then((mod) => mod.FormNotice),
		() => loadedEdit()?.FormNotice
	);
	const DraftPart = lazyPart(
		() => loadEdit().then((mod) => mod.DraftBanner),
		() => loadedEdit()?.DraftBanner
	);
	$effect(() => BarPart.want(barShown));
	$effect(() => SummaryPart.want(summaryShown));
	$effect(() => NoticePart.want(notice));
	$effect(() => DraftPart.want(form.draft));
	// The edit chunk didn't load: nothing can be saved from this page until a reload. A stored
	// draft whose offer didn't load says so on its own.
	const partFailed = $derived(
		!form.draftUnavailable &&
			(BarPart.failed || SummaryPart.failed || NoticePart.failed || DraftPart.failed)
	);

	// After the first view, while the browser is idle: the edit chunk (and on an instant form
	// instant mode), so the first edit finds it in place, even if the connection drops meanwhile.
	const shown = $derived(form.status !== 'loading' && form.status !== 'load-error');
	$effect(() => {
		if (!shown) return;
		const instant = form.mode === 'instant';
		const go = () => void preloadSettingsForm(instant).catch(() => {});
		if (typeof requestIdleCallback !== 'function') {
			const t = setTimeout(go, 500);
			return () => clearTimeout(t);
		}
		const id = requestIdleCallback(go, { timeout: 2000 });
		return () => cancelIdleCallback(id);
	});

	// The fields' on-demand controls load with the values, so they're in place at first paint.
	// After a tick, so a screen's own widgets have registered and a field isn't taken for one
	// that falls back to the JSON control.
	$effect(() => {
		const specs = form.specs;
		void tick().then(() => loadControls(specs.map(widgetFor)));
	});

	// --- saving ------------------------------------------------------------------------------

	const save = () => form.save();

	// The summary follows every save, whoever started it (the bar, ⌘/Ctrl+S, the leave dialog,
	// the wizard's Next calling `form.save()`): a failure with errors shows it and focuses it.
	$effect(() => {
		const last = form.lastSave;
		if (!last) return;
		untrack(() => {
			if (last.result.ok) summaryShown = false;
			else if (form.errors.length) {
				summaryShown = true;
				summaryFocus = ++focusSeq;
			}
		});
	});

	// Questions this form asked close with it (a navigation away, a step change).
	const asking = new AbortController();
	onDestroy(() => asking.abort());

	// --- the default disruptive confirmation (§10.10.7) --------------------------------------

	// A fallback: a screen's own disruptive handler (Network's lockout dialog, §10.10.6) wins.
	// The gate runs inside a save, so the edit chunk is in.
	$effect(() =>
		form.defaultGate('disruptive', async (c) =>
			(await loadEdit()).askDisruptive(form, c, asking.signal)
		)
	);

	// --- events ------------------------------------------------------------------------------

	// The events stream loads after the first view: nothing has changed elsewhere before it's open.
	$effect(() => {
		let off: (() => void) | undefined;
		let gone = false;
		void import('$lib/state/events.svelte').then(({ events }) => {
			if (gone) return;
			off = events.on('settings', (e) => form.received(e), {
				onResync: () => void form.resync()
			});
		});
		return () => {
			gone = true;
			off?.();
		};
	});

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

	$effect(() => {
		if (!shortcut) return;
		let off: (() => void) | undefined;
		let gone = false;
		// Loaded on demand, as the shell does (W9): the registry never enters a static import.
		void import('./save-key')
			.then(({ registerSaveKey }) =>
				registerSaveKey(
					form.id,
					() => void save(),
					() => gone
				)
			)
			.then(
				(unregister) => {
					if (gone) unregister();
					else off = unregister;
				},
				() => {}
			);
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
		let edit: Awaited<ReturnType<typeof loadEdit>>;
		try {
			edit = await loadEdit();
		} catch {
			// The question can't load: leave with a full page load, so the browser's own prompt asks.
			hardNavigate(to.href);
			return;
		}
		await edit.askToLeave(form, to, unload, asking.signal, async (href) => {
			bypass = true;
			try {
				// eslint-disable-next-line svelte/no-navigation-without-resolve -- the router's own target, already resolved
				await goto(href);
			} finally {
				bypass = false;
			}
		});
	}
</script>

<div
	class="ls-form flex flex-col gap-6"
	class:ls-form-bar={barShown}
	role="presentation"
	{onkeydown}
>
	{#if summaryShown && form.errors.length && SummaryPart.part}
		<SummaryPart.part
			{form}
			focusId={summaryFocus}
			focused={summaryFocused}
			onfocused={(id: number) => (summaryFocused = id)}
			{onReveal}
		/>
	{/if}

	{#if form.draft && DraftPart.part}
		<DraftPart.part {form} />
	{:else if form.draftUnavailable}
		<Banner kind="warning">
			<span data-testid="draft-failed">{m.settings_form_draft_failed()}</span>
			{#snippet action()}
				<!-- Browsers keep a failed module: only a reload loads it again. Unsaved edits made
				     since get the browser's own prompt from the leave guard. -->
				<Button dense onclick={() => location.reload()}>{m.settings_form_retry()}</Button>
			{/snippet}
		</Banner>
	{/if}

	{#if partFailed}
		<Banner kind="warning">
			<span data-testid="part-failed">{m.settings_form_chunk_failed()}</span>
			{#snippet action()}
				<Button dense onclick={() => location.reload()}>{m.settings_form_reload()}</Button>
			{/snippet}
		</Banner>
	{/if}

	{#if notice && NoticePart.part}
		<NoticePart.part {form} />
	{/if}

	{#if form.status === 'loading' || !labelsReady}
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

	{#if barShown && BarPart.part}
		<BarPart.part {form} signal={asking.signal} ondiscard={() => (summaryShown = false)} />
	{/if}
</div>

<style>
	/* While the bar shows, the column keeps room for it under the last field. */
	.ls-form-bar {
		padding-bottom: calc(var(--spacing) * 4);
	}
</style>
