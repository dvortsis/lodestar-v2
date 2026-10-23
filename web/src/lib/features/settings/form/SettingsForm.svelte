<script lang="ts">
	// Everything a form needs that's tied to a component's lifetime (§10.10.5): the error summary,
	// the draft and network banners, the one polite live region, the save bar, ⌘/Ctrl+S, the leave
	// guard, the `settings` subscription and the default disruptive confirmation. The screen puts
	// its groups and `SettingField`s in `children`. The bar, the summary and the questions load
	// when they're first needed.
	import { beforeNavigate, goto } from '$app/navigation';
	import { events } from '$lib/state/events.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { onDestroy, untrack, type Snippet } from 'svelte';
	import { loadControls } from './controls.svelte';
	import { lazyPart } from './lazy.svelte';
	import { preloadSettingsForm, type SettingsForm } from './form.svelte';
	import { hardNavigate, leaveDecision } from './leave';
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

	let summaryShown = $state(false);
	let summaryFocus = $state(0);
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
	const count = $derived(formatCount(form.dirtyCount, currentLocale()));

	// From the edit chunk, which the form loads with the first edit.
	const edit = () => import('./edit');
	const BarPart = lazyPart(() => edit().then((mod) => mod.SaveBar));
	const SummaryPart = lazyPart(() => edit().then((mod) => mod.ErrorSummary));
	const NoticePart = lazyPart(() => edit().then((mod) => mod.FormNotice));
	const notice = $derived(form.status === 'offline' || form.status === 'save-error');
	$effect(() => BarPart.want(barShown));
	$effect(() => SummaryPart.want(summaryShown));
	$effect(() => NoticePart.want(notice));

	// After the first view, while the browser is idle: the edit chunk (and instant mode), so the
	// first edit finds it in place, even if the connection drops meanwhile.
	const shown = $derived(form.status !== 'loading' && form.status !== 'load-error');
	$effect(() => {
		if (!shown) return;
		const go = () => void preloadSettingsForm(form.mode === 'instant').catch(() => {});
		if (typeof requestIdleCallback !== 'function') {
			const t = setTimeout(go, 500);
			return () => clearTimeout(t);
		}
		const id = requestIdleCallback(go, { timeout: 2000 });
		return () => cancelIdleCallback(id);
	});

	// The fields' on-demand controls load with the values, so they're in place at first paint.
	$effect(() => {
		void loadControls(form.specs.map(widgetFor));
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
				summaryFocus = last.id;
			}
		});
	});

	// Questions this form asked close with it (a navigation away, a step change).
	const asking = new AbortController();
	onDestroy(() => asking.abort());

	// --- the default disruptive confirmation (§10.10.7) --------------------------------------

	// A fallback: a screen's own disruptive handler (Network's lockout dialog, §10.10.6) wins.
	$effect(() =>
		form.defaultGate('disruptive', async (c) =>
			(await edit()).askDisruptive(form, c, asking.signal)
		)
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

	let barEl = $state<HTMLElement>();
	function onfocusin(e: FocusEvent) {
		if (!barEl || !(e.target instanceof HTMLElement) || barEl.contains(e.target)) return;
		const target = e.target.getBoundingClientRect();
		const top = barEl.getBoundingClientRect().top;
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
		const answer = await (await edit()).askLeave(count, asking.signal);
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
	{#if summaryShown && form.errors.length && SummaryPart.part}
		<SummaryPart.part {form} focusId={summaryFocus} {onReveal} />
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

	{#if notice && NoticePart.part}
		<NoticePart.part {form} />
	{/if}

	{#if form.status === 'loading'}
		<p class="text-muted-text" aria-busy="true">{m.settings_form_loading()}</p>
	{:else if form.status === 'load-error'}
		<!-- Inline: a form that couldn't load may not reach the edit chunk either. -->
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
		<BarPart.part
			{form}
			signal={asking.signal}
			ondiscard={() => (summaryShown = false)}
			bind:el={barEl}
		/>
	{/if}
</div>

<style>
	/* While the bar shows, the column keeps room for it under the last field. */
	.ls-form-bar {
		padding-bottom: calc(var(--spacing) * 4);
	}
</style>
