<script lang="ts">
	// Settings → Preferences → Appearance (§10.10.8, §10.18). Preferences apply at once and can be
	// undone; there is no Save. The theme picker and the custom theme editor load when opened.
	import { onMount, tick, type Component, type ComponentProps } from 'svelte';
	import { ApiError, api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import type { AdultConfirm } from '../filters/flows/adult.svelte';
	import { SHOW_ADULT_ENV, showAdultOff } from './show-adult';
	import {
		CONTENT,
		DEFAULTS,
		DENSITIES,
		MODES,
		MOTIONS,
		applyLook,
		load,
		save,
		Unreadable,
		type Appearance,
		type Loaded,
		type Name
	} from './prefs';
	import './appearance-screen.css';
	import { applyTheme, loadThemes, markSave, readCustom, writeCustom } from './themes';

	let status = $state<'loading' | 'ready' | 'failed'>('loading');
	let prefs = $state<Appearance>({ ...DEFAULTS });
	/** What the server last confirmed; `prefs` is this plus any change still saving. */
	let saved = $state.raw<Appearance>({ ...DEFAULTS });
	let locks = $state<Loaded['locks']>({});
	let revision = 0;
	let failed = $state(false);
	let themesFailed = $state(false);
	let undo = $state<Partial<Appearance> | null>(null);
	let picker = $state<Component<ComponentProps<typeof import('./ThemePicker.svelte').default>>>();
	let pickerFailed = $state(false);
	let missing = $state(false);
	/** Saves go one at a time, each with the revision the last one returned. */
	let queue: Promise<void> = Promise.resolve();
	let saving = 0;
	/** Changes still queued: a re-read lays them back over the server's values. */
	let pending: Partial<Appearance>[] = [];
	let systemDark = $state(false);
	const dark = $derived(prefs.mode === 'dark' || (prefs.mode === 'system' && systemDark));

	let adult = $state(false);
	let adultLock = $state<string | null>(null);
	let adultKey = $state(0);
	let adultState = $state<'unknown' | 'ready' | 'failed'>('unknown');
	type AdultFlow = Awaited<ReturnType<typeof import('../filters/flows-door').loadAdult>>;
	let adultFlow = $state.raw<{ flow: AdultFlow; confirm: AdultConfirm }>();

	const locked = (n: Name) => n in locks;
	const lockedNames = $derived(
		Object.entries(locks)
			.map(([n, env]) => (env ? `${n} (${env})` : n))
			.join(', ')
	);

	const modeOptions = $derived(
		MODES.map((v) => ({
			value: v,
			disabled: locked('mode'),
			label: {
				system: m.appearance_mode_system(),
				light: m.appearance_mode_light(),
				dark: m.appearance_mode_dark()
			}[v]
		}))
	);
	const densityOptions = $derived(
		DENSITIES.map((v) => ({
			value: v,
			disabled: locked('density'),
			label: {
				compact: m.appearance_density_compact(),
				comfortable: m.appearance_density_comfortable(),
				cards: m.appearance_density_cards()
			}[v]
		}))
	);
	const motionOptions = $derived(
		MOTIONS.map((v) => ({
			value: v,
			disabled: locked('motion'),
			label: {
				system: m.appearance_motion_system(),
				reduced: m.appearance_motion_reduced(),
				full: m.appearance_motion_full()
			}[v]
		}))
	);

	async function apply(p: Appearance) {
		applyLook(p);
		try {
			themesFailed = false;
			missing = (await applyTheme(p)).missing.some((id) => id.startsWith('custom-'));
		} catch {
			themesFailed = true;
		}
	}

	async function refresh() {
		try {
			const res = await load();
			if (!res) {
				status = 'failed';
				return;
			}
			({ prefs, locks, revision } = res);
			saved = res.prefs;
			for (const p of pending) prefs = { ...prefs, ...p };
			status = 'ready';
			void apply(prefs);
		} catch {
			if (status === 'loading') status = 'failed';
		}
	}

	/** Applies a change at once, then saves it after any save already under way. */
	function change(patch: Partial<Appearance>, undoing = false): Promise<void> {
		const keys = Object.keys(patch) as Name[];
		if (keys.some(locked)) return Promise.resolve();
		const pick = (p: Appearance) =>
			Object.fromEntries(keys.map((k) => [k, p[k]])) as Partial<Appearance>;
		prefs = { ...$state.snapshot(prefs), ...patch } as Appearance;
		failed = false;
		undo = null;
		void apply(prefs);
		markSave(true);
		saving++;
		pending.push(patch);
		queue = queue.then(async () => {
			// What the server held before this change: Undo never restores a value that never saved.
			const was = pick(saved);
			try {
				revision = await save(patch, revision);
				saved = { ...saved, ...patch };
				failed = false;
				// Offered only once nothing later is queued, so Undo always belongs to the latest
				// change. Undoing is a change of its own and isn't itself undoable.
				if (!undoing && saving === 1) undo = was;
			} catch (e) {
				pending = pending.filter((p) => p !== patch);
				// A 2xx that can't be read may have landed: re-read rather than say it failed.
				if (e instanceof Unreadable) return void (await refresh());
				// Put back only the fields this change set, and only if nothing newer replaced them.
				const now = $state.snapshot(prefs) as Record<Name, unknown>;
				for (const k of keys) if (now[k] === patch[k]) now[k] = saved[k];
				prefs = now as unknown as Appearance;
				void apply(prefs);
				if (e instanceof ApiError && e.code === 'stale_revision') await refresh();
				failed = !(e instanceof ApiError && e.status === 401);
			} finally {
				pending = pending.filter((p) => p !== patch);
				saving--;
				markSave(false);
			}
		});
		return queue;
	}

	/** Where each field is changed: focus goes there after Undo, which removes its own button. */
	const THEME = '[aria-labelledby="ap-theme"]';
	const CONTROL: Record<Name, string[]> = {
		theme: [`${THEME} input[type="radio"]:checked`, '#ap-filter'],
		theme_dark: ['#ap-dark', `${THEME} [role="switch"]`, '#ap-filter'],
		mode: [`${THEME} [role="radio"][aria-checked="true"]`],
		density: ['[aria-labelledby="ap-density"] [role="radio"][aria-checked="true"]'],
		motion: ['[aria-labelledby="ap-motion"] [role="radio"][aria-checked="true"]'],
		illustrations: ['[data-testid="ap-illustrations"] [role="switch"]']
	};

	function undoLast() {
		const back = undo!;
		undo = null;
		void change(back, true);
		const name = Object.keys(back)[0] as Name;
		void tick().then(() => {
			for (const sel of CONTROL[name]) {
				const el = document.querySelector<HTMLElement>(sel);
				if (el) return el.focus();
			}
		});
	}

	async function loadAdult() {
		try {
			const res = (await api.get('/settings/{section}', CONTENT)) as unknown as {
				fields: { show_adult?: { value?: unknown; env?: string | null; read_only?: boolean } };
			};
			const f = res.fields.show_adult;
			adult = f?.value === true;
			adultLock = f?.env ? f.env : f?.read_only ? SHOW_ADULT_ENV : null;
			adultState = 'ready';
		} catch {
			adultState = 'failed';
		}
	}

	async function toggleAdult(on: boolean) {
		adultState = 'ready';
		let loaded = adultFlow;
		try {
			// The 18+ dialog (Bits UI overlays) and turning it on load with the first turn-on, through
			// the flows' door (X-flows-door-2), so none of the flows is in this first view. Turning it
			// off asks nothing and never waits on that load.
			if (on && !loaded) {
				const flow = await (await import('../filters/flows-door')).loadAdult();
				loaded = adultFlow ??= { flow, confirm: flow.createAdultConfirm() };
			}
		} catch {
			adultState = 'failed';
			adultKey++;
			return;
		}
		const res =
			on && loaded ? await loaded.flow.setShowAdult(on, loaded.confirm) : await showAdultOff();
		if (res.ok) adult = on;
		else if (res.error) adultState = 'failed';
		// Cancelled or failed: the switch shows what the server has.
		if (!res.ok) adultKey++;
	}

	async function openPicker() {
		pickerFailed = false;
		try {
			picker = (await import('./ThemePicker.svelte')).default;
		} catch {
			pickerFailed = true;
		}
	}

	onMount(() => {
		void refresh();
		void loadAdult();
		// "System" follows the device's colour scheme live (§10.18).
		const query = matchMedia('(prefers-color-scheme: dark)');
		systemDark = query.matches;
		const follow = () => {
			systemDark = query.matches;
			if (prefs.mode === 'system') void apply(prefs);
		};
		query.addEventListener('change', follow);
		return () => query.removeEventListener('change', follow);
	});
</script>

<div class="as-root" data-testid="appearance-screen" aria-busy={status === 'loading'}>
	<h1 class="as-h1">{m.appearance_title()}</h1>
	<p class="as-text">{m.appearance_lead()}</p>
	{#if status === 'failed'}
		<Banner kind="error">{m.appearance_load_failed()}</Banner>
	{:else if status === 'ready'}
		{#if lockedNames}<Banner kind="info">{m.appearance_locked({ fields: lockedNames })}</Banner
			>{/if}
		{#if missing}<Banner kind="info">{m.appearance_custom_missing()}</Banner>{/if}
		{#if failed}<Banner kind="error">{m.appearance_save_failed()}</Banner>{/if}
		{#if themesFailed}<Banner kind="error">{m.appearance_picker_failed()}</Banner>{/if}
		<div role="status" class="as-status">
			{#if undo && !Object.keys(undo).some((n) => locked(n as Name))}
				{m.appearance_saved()}
				<Button variant="quiet" onclick={undoLast}>{m.appearance_undo()}</Button>
			{/if}
		</div>

		<section class="as-section" aria-labelledby="ap-theme">
			<h2 id="ap-theme" class="as-h2">{m.appearance_theme()}</h2>
			<p class="as-text">{m.appearance_theme_help()}</p>
			<SegmentedControl
				class="self-start"
				label={m.appearance_mode()}
				options={modeOptions}
				value={prefs.mode}
				onchange={(v) => void change({ mode: v as Appearance['mode'] })}
			/>
			{#if picker}
				{@const Picker = picker}
				<Picker
					{prefs}
					{dark}
					locked={locked('theme') || locked('theme_dark')}
					themes={loadThemes}
					custom={readCustom}
					write={writeCustom}
					onpick={(p) => void change(p)}
				/>
			{:else}
				<p class="as-body">
					{m.appearance_current({ theme: prefs.theme })}
				</p>
				<div>
					<Button onclick={() => void openPicker()}>{m.appearance_browse()}</Button>
				</div>
				{#if pickerFailed}<Banner kind="error">{m.appearance_picker_failed()}</Banner>{/if}
			{/if}
		</section>

		<section class="as-section" aria-labelledby="ap-density">
			<h2 id="ap-density" class="as-h2">{m.appearance_density()}</h2>
			<SegmentedControl
				class="self-start"
				label={m.appearance_density()}
				options={densityOptions}
				value={prefs.density}
				onchange={(v) => void change({ density: v as Appearance['density'] })}
			/>
		</section>

		<section class="as-section" aria-labelledby="ap-motion">
			<h2 id="ap-motion" class="as-h2">{m.appearance_motion()}</h2>
			<p class="as-text">{m.appearance_motion_help()}</p>
			<SegmentedControl
				class="self-start"
				label={m.appearance_motion()}
				options={motionOptions}
				value={prefs.motion}
				onchange={(v) => void change({ motion: v as Appearance['motion'] })}
			/>
		</section>

		<section class="as-section" data-testid="ap-illustrations">
			<Toggle
				label={m.appearance_illustrations()}
				checked={prefs.illustrations}
				disabled={locked('illustrations')}
				onchange={(e) => void change({ illustrations: e.currentTarget.checked })}
			/>
			<p class="as-text">{m.appearance_illustrations_help()}</p>
			{#if (navigator as Navigator & { connection?: { saveData?: boolean } }).connection?.saveData}<p
					class="as-text"
				>
					{m.appearance_illustrations_save_data()}
				</p>{/if}
		</section>

		<section class="as-section" aria-labelledby="ap-adult">
			<h2 id="ap-adult" class="as-h2">{m.appearance_adult()}</h2>
			<p class="as-text">{m.appearance_adult_help()}</p>
			{#if adultState === 'failed'}<Banner kind="error">{m.appearance_adult_failed()}</Banner>{/if}
			{#if adultLock}<p class="as-text">
					{m.appearance_adult_locked({ variable: adultLock })}
				</p>{/if}
			{#if adultState !== 'unknown'}
				{#key adultKey}
					<Toggle
						label={m.appearance_adult_label()}
						checked={adult}
						disabled={!!adultLock}
						onchange={(e) => void toggleAdult(e.currentTarget.checked)}
					/>
				{/key}
			{/if}
			{#if adultFlow}<adultFlow.flow.AdultConfirmDialog confirm={adultFlow.confirm} />{/if}
		</section>
	{/if}
</div>
