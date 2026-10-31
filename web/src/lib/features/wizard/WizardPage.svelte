<script lang="ts">
	// The first-run wizard's shell (§10.10.10): stepped progress, one primary action per step, Back
	// and Skip on the optional ones. Steps 1 and 2 are done in /setup; the rest load on demand, one
	// chunk each, and share this frame. Progress sits in sessionStorage so a reload resumes, and
	// nothing else in the app waits on it.
	import { tick, type Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { formatCount } from '$lib/format';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import IconCheck from '~icons/ph/check-circle';
	import IconSkip from '~icons/ph/skip-forward';
	import IconHere from '~icons/ph/arrow-circle-right';
	import IconLater from '~icons/ph/circle';
	import {
		STEPS,
		advance,
		back,
		readProgress,
		stepIndex,
		writeProgress,
		type Progress,
		type StepId
	} from './steps';

	interface Body {
		save?: () => Promise<boolean>;
	}
	type Loaded = Component<{ progress: Progress }, Body>;

	const LOADERS: Record<StepId, () => Promise<{ default: unknown }>> = {
		network: () => import('./NetworkStep.svelte'),
		how: () => import('./HowStep.svelte'),
		intensity: () => import('./IntensityStep.svelte'),
		storage: () => import('./StorageStep.svelte'),
		updates: () => import('./UpdatesStep.svelte'),
		done: () => import('./DoneStep.svelte')
	};

	const titles: Record<StepId, () => string> = {
		network: () => m.wizard_step_network(),
		how: () => m.wizard_step_how(),
		intensity: () => m.wizard_step_intensity(),
		storage: () => m.wizard_step_storage(),
		updates: () => m.wizard_step_updates(),
		done: () => m.wizard_step_done()
	};

	let progress = $state<Progress>(readProgress());
	const info = $derived(STEPS[stepIndex(progress.step)]!);
	const index = $derived(stepIndex(progress.step));
	let Step = $state<Loaded | undefined>();
	let failed = $state(false);
	let body = $state<Body>();
	let working = $state(false);
	let heading = $state<HTMLElement>();
	let generation = 0;

	// Loads the current step's chunk; a step whose chunk didn't load can only be retried by a
	// reload (browsers keep a failed module).
	$effect(() => {
		const id = progress.step;
		const mine = ++generation;
		Step = undefined;
		failed = false;
		LOADERS[id]().then(
			(mod) => {
				if (mine !== generation) return;
				Step = mod.default as Loaded;
			},
			() => {
				if (mine === generation) failed = true;
			}
		);
		return () => {
			++generation;
		};
	});

	function go(next: Progress) {
		progress = next;
		writeProgress(next);
		void tick().then(() => heading?.focus());
	}

	async function next() {
		if (working) return;
		working = true;
		try {
			// Steps that save say whether they did; the others just move on.
			const ok = body?.save ? await body.save() : true;
			if (ok) go(advance(progress, 'done'));
		} finally {
			working = false;
		}
	}
	const skip = () => go(advance(progress, 'skipped'));
	const previous = () => go(back(progress));

	const status = (id: StepId): 'done' | 'skipped' | 'current' | 'later' =>
		id === progress.step
			? 'current'
			: progress.skipped.includes(id)
				? 'skipped'
				: progress.done.includes(id) || stepIndex(id) < index
					? 'done'
					: 'later';
	const statusText = {
		done: () => m.wizard_status_done(),
		skipped: () => m.wizard_status_skipped(),
		current: () => m.wizard_status_current(),
		later: () => m.wizard_status_later()
	};
	const statusIcon = { done: IconCheck, skipped: IconSkip, current: IconHere, later: IconLater };
	const primary = $derived(
		progress.step === 'network' || progress.step === 'how'
			? m.wizard_continue()
			: m.wizard_save_continue()
	);
</script>

<div class="mx-auto flex max-w-2xl flex-col gap-6 px-4 py-8" data-testid="wizard">
	<header class="flex flex-col gap-1">
		<h1 class="w-fit max-w-full text-display font-semibold text-base-content">
			{m.wizard_title()}
		</h1>
		<p class="text-body text-muted-text">{m.wizard_lead()}</p>
	</header>

	<nav aria-label={m.wizard_progress()}>
		<ol class="flex flex-wrap gap-x-4 gap-y-2 text-label">
			{#each [m.auth_setup_step_token(), m.auth_setup_step_password()] as past (past)}
				<li class="flex items-center gap-1 text-muted-text">
					<IconCheck aria-hidden="true" class="text-success" />
					<span>{past}</span>
					<span class="sr-only">{statusText.done()}</span>
				</li>
			{/each}
			{#each STEPS as step, i (step.id)}
				{@const s = status(step.id)}
				{@const Icon = statusIcon[s]}
				<li
					class="flex items-center gap-1 {s === 'current'
						? 'font-semibold text-base-content'
						: 'text-muted-text'}"
					aria-current={s === 'current' ? 'step' : undefined}
					data-testid="wizard-step-{step.id}"
					data-state={s}
				>
					<Icon aria-hidden="true" />
					<span>{formatCount(i + 3)}. {titles[step.id]()}</span>
					<span class="text-muted-text">· {statusText[s]()}</span>
				</li>
			{/each}
		</ol>
	</nav>

	<section class="flex flex-col gap-4" aria-labelledby="wizard-heading">
		<h2
			id="wizard-heading"
			bind:this={heading}
			tabindex="-1"
			class="w-fit max-w-full text-title font-semibold text-base-content"
		>
			{titles[progress.step]()}
		</h2>
		{#if failed}
			<Banner kind="error">
				{m.wizard_load_failed()}
				{#snippet action()}
					<Button dense onclick={() => location.reload()}>{m.wizard_reload()}</Button>
				{/snippet}
			</Banner>
		{:else if Step}
			<Step bind:this={body} {progress} />
		{:else}
			<p class="text-muted-text" aria-busy="true">{m.wizard_loading()}</p>
		{/if}
	</section>

	{#if progress.step !== 'done'}
		<div class="flex flex-wrap items-center gap-3">
			{#if index > 0}
				<Button onclick={previous} disabled={working}>{m.wizard_back()}</Button>
			{/if}
			<Button variant="primary" onclick={() => void next()} loading={working} disabled={!Step}>
				{primary}
			</Button>
			{#if info.optional}
				<Button variant="quiet" onclick={skip} disabled={working}>{m.wizard_skip()}</Button>
			{/if}
		</div>
	{/if}
</div>

<style>
	/* The step heading takes focus on each step; the ring hugs its text, like the page heading. */
	#wizard-heading:focus-visible {
		outline: var(--focus-ring-width) solid var(--color-primary);
		outline-offset: var(--focus-ring-offset);
	}
</style>
