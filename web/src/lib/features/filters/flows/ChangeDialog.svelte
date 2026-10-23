<script lang="ts">
	// One filtering change, start to finish (§20.10, §20.11): the preview's progress, the exposure
	// summary, the server's effect warning, the password when the server asks for it, Apply /
	// Shadow for N days / Cancel, then "What changed". It renders the flow's state and decides
	// nothing: whether the password is needed is the assessment's `confirm`.
	//
	// The password lives only in this component's input. It is cleared as soon as it is sent, and
	// whenever the flow leaves `confirm` (any outcome, a variant, a re-preview, close).
	import { onDestroy, tick, type Snippet } from 'svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { canShadow } from './changes';
	import EffectWarning from './EffectWarning.svelte';
	import { errorText } from './errors';
	import type { ChangeFlow } from './flow.svelte';
	import type { FlowState } from './machine';
	import ImpactSummary from './ImpactSummary.svelte';
	import type { ApplyResponse, Assessment, Change } from './types';
	import WhatChanged from './WhatChanged.svelte';

	interface Props {
		flow: ChangeFlow;
		title: string;
		/** The Apply button's label, naming the action ("Turn off CS-4a"). */
		applyLabel?: string;
		/** Offer "Shadow for N days" (only for changes that take `shadow_days`). */
		shadowDays?: number;
		onapplied?: (response: ApplyResponse) => void;
		onclose?: () => void;
		/** Keep the dialog open before the flow starts (a form step, shown by `form`). */
		open?: boolean;
		/** The form step, shown while the flow is idle or closed. */
		form?: Snippet;
		/** Its footer actions. */
		formFooter?: Snippet;
		/** Shown above the summary from the preview on (e.g. the CSX-5 strength switch). */
		extra?: Snippet;
		/** Replaces the default exposure summary. */
		summary?: Snippet<[Assessment]>;
		/** Replaces "What changed" (e.g. a revealed name). */
		result?: Snippet<[ApplyResponse]>;
	}

	let {
		flow,
		title,
		applyLabel,
		shadowDays,
		onapplied,
		onclose,
		open = $bindable(false),
		form,
		formFooter,
		extra,
		summary,
		result
	}: Props = $props();

	const uid = $props.id();
	const formId = `${uid}-form`;
	const warningId = `${uid}-warning`;

	let password = $state('');
	let bodyEl = $state<HTMLElement | null>(null);
	let doneEl = $state<HTMLElement | null>(null);
	let failedEl = $state<HTMLElement | null>(null);
	/** A close asked for while applying: close once the result is in (§10.13). */
	let closeAfterApply = $state(false);
	let clock = $state(Date.now());

	const s = $derived(flow.state);
	const active = $derived(s.kind !== 'idle' && s.kind !== 'closed');
	const isOpen = $derived(open || active);
	// Bound to the Dialog, so a close asked for while applying can be undone in the same tick.
	let dialogOpen = $derived(isOpen);

	const passwordEl = () =>
		bodyEl?.querySelector<HTMLInputElement>('input[name="password"]') ?? null;
	const change = $derived<Change | null>(
		s.kind === 'review' || s.kind === 'confirm' || s.kind === 'applying' ? s.change : null
	);
	const assessment = $derived<Assessment | null>(
		s.kind === 'review' || s.kind === 'confirm' || s.kind === 'applying' ? s.assessment : null
	);
	/** The server asked for the password for the change on screen. */
	const asksPassword = $derived(
		s.kind === 'confirm' || (s.kind === 'applying' && s.assessment.confirm)
	);
	const waitFor = $derived(
		s.kind === 'confirm' && s.retryAt !== undefined
			? Math.max(0, Math.ceil((s.retryAt - clock) / 1000))
			: 0
	);
	const shadowing = $derived(
		change !== null && canShadow(change) && typeof change.shadow_days === 'number'
	);
	const offerShadow = $derived(
		shadowDays !== undefined && change !== null && canShadow(change) && s.kind !== 'applying'
	);
	const primaryLabel = $derived(
		shadowing && change && canShadow(change)
			? m.filters_flows_shadow({ days: formatCount(change.shadow_days, currentLocale()) })
			: (applyLabel ?? m.filters_flows_apply())
	);

	// Leaving `confirm` (sent, refused, re-previewed, closed) always drops the typed password.
	$effect.pre(() => {
		if (s.kind !== 'confirm') password = '';
	});

	// The countdown of a rate-limited confirm is text that moves once a second.
	$effect.pre(() => {
		if (s.kind !== 'confirm' || s.retryAt === undefined) return;
		clock = Date.now();
		const t = setInterval(() => (clock = Date.now()), 1000);
		return () => clearInterval(t);
	});

	// Progress, announced politely at most every 2 s.
	let announced = $state('');
	let lastAnnounce = 0;
	const phases = {
		reading: m.filters_flows_phase_reading,
		quarantine: m.filters_flows_phase_quarantine,
		tombstones: m.filters_flows_phase_tombstones,
		done: m.filters_flows_phase_done
	};
	/** A phase this build doesn't know (a newer server) reads as plain progress. */
	const phaseText = (p: string) =>
		(phases as Record<string, (() => string) | undefined>)[p]?.() ?? m.filters_flows_previewing();
	const progressText = $derived(
		s.kind === 'previewing' && s.progress
			? m.filters_flows_progress_announce({
					phase: phaseText(s.progress.phase),
					done: formatCount(s.progress.done, currentLocale()),
					total: formatCount(s.progress.total, currentLocale())
				})
			: ''
	);
	$effect(() => {
		if (!progressText) return;
		const now = Date.now();
		if (now - lastAnnounce < 2000) return;
		lastAnnounce = now;
		announced = progressText;
	});

	function titleEl(): HTMLElement | null {
		const dialog = bodyEl?.closest('[role="dialog"]');
		const id = dialog?.getAttribute('aria-labelledby');
		const el = id ? document.getElementById(id) : null;
		if (el && !el.hasAttribute('tabindex')) el.tabIndex = -1;
		return el;
	}

	function initialFocus() {
		return s.kind === 'confirm' ? passwordEl() : titleEl();
	}

	// Focus follows the state when what had it is gone: the password field when the server asks
	// for it, the result or the error once they arrive, else the title.
	let lastKind: FlowState['kind'] | null = null;
	$effect(() => {
		const kind = s.kind;
		const first = lastKind === null;
		if (kind === lastKind) return;
		lastKind = kind;
		if (first) return;
		tick().then(() => {
			if (!bodyEl) return;
			const target =
				kind === 'confirm'
					? passwordEl()
					: kind === 'done'
						? doneEl
						: kind === 'failed'
							? failedEl
							: null;
			const within = bodyEl.closest('[role="dialog"]')?.contains(document.activeElement);
			if (target) target.focus();
			else if (!within) titleEl()?.focus();
		});
	});

	let reported: ApplyResponse | null = null;
	$effect(() => {
		if (s.kind === 'done' && s.response !== reported) {
			reported = s.response;
			onapplied?.(s.response);
		}
	});

	// What had focus when the dialog opened, read before the dialog moves focus inside (the modal's
	// own record runs after that when it opens from code), so close() can restore it.
	let openedFrom: Element | null = null;
	let wasOpen = false;
	$effect.pre(() => {
		if (isOpen && !wasOpen) openedFrom = document.activeElement;
		wasOpen = isOpen;
	});

	// A close asked for while applying waits for the result, then closes.
	$effect(() => {
		if (!closeAfterApply || s.kind === 'applying') return;
		closeAfterApply = false;
		close();
	});

	// The flow can close on its own (a job cancelled elsewhere, a sign-out): that's a close too.
	let wasActive = false;
	$effect(() => {
		const now = active;
		if (wasActive && !now && s.kind === 'closed' && !open) close();
		wasActive = now;
	});

	function close() {
		// Our own cancel() below must not read as the flow closing on its own (a second close).
		wasActive = false;
		password = '';
		reported = null;
		open = false;
		flow.cancel();
		onclose?.();
		// Back to the invoker (§10.13), unless something outside the dialog took focus meanwhile.
		const back = openedFrom;
		setTimeout(() => {
			const now = document.activeElement;
			const lost =
				!now || now === document.body || !now.isConnected || !!now.closest('[role="dialog"]');
			if (lost && back instanceof HTMLElement && back.isConnected) back.focus();
		});
	}

	function onOpenChange(next: boolean) {
		if (next) return;
		if (flow.state.kind === 'applying') {
			// Esc, the close button or a click outside: keep the dialog until the answer is in.
			closeAfterApply = true;
			dialogOpen = true;
			return;
		}
		close();
	}

	function submit(event?: SubmitEvent) {
		event?.preventDefault();
		if (s.kind === 'review') {
			flow.apply();
		} else if (s.kind === 'confirm') {
			if (waitFor > 0) return;
			const pw = password;
			password = '';
			flow.confirm(pw);
			if (pw === '') passwordEl()?.focus();
		}
	}

	function toggleShadow() {
		if (!change || !canShadow(change) || shadowDays === undefined) return;
		password = '';
		if (shadowing) {
			const rest = { ...change };
			delete rest.shadow_days;
			flow.variant(rest);
		} else {
			flow.variant({ ...change, shadow_days: shadowDays });
		}
	}

	onDestroy(() => {
		password = '';
		const k = flow.state.kind;
		if (k !== 'idle' && k !== 'closed' && k !== 'applying') flow.cancel();
	});

	const passwordError = $derived(
		s.kind === 'confirm' && s.error === 'empty'
			? m.filters_flows_error_empty()
			: s.kind === 'confirm' && s.error === 'bad_password'
				? m.filters_flows_error_bad_password()
				: undefined
	);
	const busy = $derived(s.kind === 'applying');
	const locked = $derived(waitFor > 0);
	let lockedFor = $state(0);
	let wasLocked = false;
	$effect.pre(() => {
		if (locked && !wasLocked) lockedFor = waitFor;
		wasLocked = locked;
	});
</script>

<Dialog
	bind:open={dialogOpen}
	{title}
	closeLabel={m.filters_flows_close()}
	{onOpenChange}
	{initialFocus}
>
	<div
		bind:this={bodyEl}
		class="flex flex-col gap-4"
		data-testid="change-dialog"
		data-state={s.kind}
	>
		<!-- Polite progress (throttled) and assertive errors; the visible text is below. -->
		<p class="sr-only" role="status" aria-live="polite">{announced}</p>

		{#if !active}
			{@render form?.()}
		{:else if s.kind === 'done'}
			<section bind:this={doneEl} tabindex="-1" aria-label={m.filters_flows_done_title()}>
				{#if result}{@render result(s.response)}{:else}
					<WhatChanged response={s.response} previewed={s.previewed} />
				{/if}
			</section>
		{:else if s.kind === 'failed'}
			<div bind:this={failedEl} tabindex="-1">
				<Banner kind="error" title={m.filters_flows_failed_title()}>
					<span data-testid="change-error">{errorText(s.error)}</span>
				</Banner>
			</div>
		{:else}
			{@render extra?.()}
			{#if s.kind === 'previewing'}
				<div class="flex flex-col gap-2" data-testid="change-previewing">
					<p>{m.filters_flows_previewing()}</p>
					{#if s.progress}
						<p class="text-small text-muted-text">
							{phaseText(s.progress.phase)} ·
							<span data-testid="change-progress"
								>{m.filters_flows_progress({
									done: formatCount(s.progress.done, currentLocale()),
									total: formatCount(s.progress.total, currentLocale())
								})}</span
							>
						</p>
						<progress
							class="progress w-full"
							value={s.progress.done}
							max={Math.max(1, s.progress.total)}
							aria-label={m.filters_flows_previewing()}
						></progress>
					{/if}
				</div>
			{:else if assessment}
				{#if s.kind === 'confirm' && s.changed}
					<Banner kind="warning"
						><span data-testid="change-changed">{m.filters_flows_changed()}</span></Banner
					>
				{/if}
				{#if summary}{@render summary(assessment)}{:else}
					<ImpactSummary preview={assessment.preview} />
				{/if}
				{#if shadowing && change && canShadow(change)}
					<p class="text-small text-muted-text" data-testid="change-shadow-note">
						{m.filters_flows_shadow_note({
							days: formatCount(change.shadow_days, currentLocale())
						})}
					</p>
				{/if}
				{#if assessment.warning !== null}
					<EffectWarning id={warningId} text={assessment.warning} password={asksPassword} />
				{/if}
				<form id={formId} method="post" novalidate onsubmit={submit}>
					{#if asksPassword}
						<!-- For password managers: the instance has one account, so the name is fixed. -->
						<input
							type="text"
							name="username"
							autocomplete="username"
							value="admin"
							hidden
							readonly
						/>
						<Field label={m.filters_flows_password_label()} error={passwordError} required>
							{#snippet children(props)}
								<Input
									{...props}
									bind:value={password}
									aria-describedby={[
										assessment.warning !== null ? warningId : '',
										props['aria-describedby'] ?? ''
									]
										.filter(Boolean)
										.join(' ') || undefined}
									type="password"
									name="password"
									autocomplete="current-password"
									readonly={busy || undefined}
									data-testid="change-password"
								/>
							{/snippet}
						</Field>
					{/if}
				</form>
				{#if s.kind === 'confirm' && s.error === 'rate_limited'}
					{#if locked}
						<Banner kind="error" title={m.filters_flows_error_rate_limited()}>
							<span aria-hidden="true" data-testid="change-countdown"
								>{m.filters_flows_countdown({
									seconds: formatCount(waitFor, currentLocale())
								})}</span
							>
							<!-- The moving digits are hidden; the wait is read once, as it started. -->
							<span class="sr-only"
								>{m.filters_flows_countdown({
									seconds: formatCount(lockedFor, currentLocale())
								})}</span
							>
						</Banner>
					{:else}
						<Banner kind="info">{m.filters_flows_unlocked()}</Banner>
					{/if}
				{/if}
				{#if busy}<p class="sr-only" role="status">{m.filters_flows_applying()}</p>{/if}
			{/if}
		{/if}
		{#if passwordError}<p class="sr-only" role="alert">{passwordError}</p>{/if}
	</div>

	{#snippet footer()}
		{#if !active}
			{#if formFooter}{@render formFooter()}{/if}
		{/if}
		{#if offerShadow}
			<Button onclick={toggleShadow} data-testid="change-shadow">
				{shadowing
					? m.filters_flows_shadow_off()
					: m.filters_flows_shadow({ days: formatCount(shadowDays, currentLocale()) })}
			</Button>
		{/if}
		{#if s.kind === 'failed' && s.canRetry}
			<Button onclick={() => flow.retry()} data-testid="change-retry"
				>{m.filters_flows_retry()}</Button
			>
		{/if}
		<Button variant="quiet" onclick={() => onOpenChange(false)} data-testid="change-cancel">
			{s.kind === 'done' || s.kind === 'failed'
				? m.filters_flows_close()
				: m.filters_flows_cancel()}
		</Button>
		{#if s.kind === 'review' || s.kind === 'confirm' || s.kind === 'applying'}
			<Button
				variant="primary"
				type="submit"
				form={formId}
				loading={busy}
				aria-disabled={busy || locked || undefined}
				aria-describedby={busy ? `${uid}-busy` : undefined}
				data-testid="change-apply"
			>
				{primaryLabel}
			</Button>
			{#if busy}<span id="{uid}-busy" class="sr-only">{m.filters_flows_busy()}</span>{/if}
		{/if}
	{/snippet}
</Dialog>
