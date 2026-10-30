<script lang="ts">
	// Settings → Preferences → Appearance (§10.10.8, §10.18). Preferences apply at once and can be
	// undone; there is no Save. The theme picker and the custom theme editor load when opened.
	import { onMount, type Component, type ComponentProps } from 'svelte';
	import { ApiError, api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import {
		CONTENT,
		DEFAULTS,
		DENSITIES,
		MODES,
		MOTIONS,
		applyLook,
		load,
		prefersDark,
		save,
		type Appearance,
		type Loaded,
		type Name
	} from './prefs';
	import './appearance-screen.css';
	import { applyTheme, loadThemes, readCustom, writeCustom } from './themes';

	let status = $state<'loading' | 'ready' | 'failed'>('loading');
	let prefs = $state<Appearance>({ ...DEFAULTS });
	let locks = $state<Loaded['locks']>({});
	let revision = 0;
	let failed = $state(false);
	let undo = $state<Partial<Appearance> | null>(null);
	let picker = $state<Component<ComponentProps<typeof import('./ThemePicker.svelte').default>>>();
	let pickerFailed = $state(false);
	let missing = $state(false);

	let adult = $state(false);
	let adultRevision = 0;
	let adultState = $state<'unknown' | 'ready' | 'asking' | 'failed'>('unknown');

	const modeOptions = $derived(
		MODES.map((v) => ({
			value: v,
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
			missing = (await applyTheme(p)).missing.some((id) => id.startsWith('custom-'));
		} catch {
			failed = true;
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
			status = 'ready';
			void apply(prefs);
		} catch {
			if (status === 'loading') status = 'failed';
		}
	}

	/** Applies a change at once, saves it, and keeps what undoes it. */
	async function change(patch: Partial<Appearance>) {
		if ((Object.keys(patch) as Name[]).some((k) => k in locks)) return;
		const before = Object.fromEntries(
			(Object.keys(patch) as Name[]).map((k) => [k, prefs[k]])
		) as Partial<Appearance>;
		const next = { ...$state.snapshot(prefs), ...patch } as Appearance;
		prefs = next;
		failed = false;
		void apply(next);
		try {
			revision = await save(patch, revision);
			undo = before;
		} catch (e) {
			prefs = { ...next, ...before };
			void apply(prefs);
			if (e instanceof ApiError && e.code === 'stale_revision') await refresh();
			failed = !(e instanceof ApiError && e.status === 401);
		}
	}

	async function loadAdult() {
		try {
			const res = (await api.get('/settings/{section}', CONTENT)) as unknown as {
				fields: { show_adult?: { value?: unknown } };
				revision: number;
			};
			adult = res.fields.show_adult?.value === true;
			adultRevision = res.revision;
			adultState = 'ready';
		} catch {
			adultState = 'failed';
		}
	}

	async function setAdult(on: boolean) {
		try {
			const res = await api.patch('/settings/{section}', {
				...CONTENT,
				body: { show_adult: on, if_revision: adultRevision } as unknown as Record<string, never>
			});
			adultRevision = (res as unknown as { revision: number }).revision;
			adult = on;
			adultState = 'ready';
		} catch (e) {
			if (e instanceof ApiError && e.code === 'stale_revision') await loadAdult();
			else adultState = 'failed';
		}
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
	});
</script>

<div class="as-root" data-testid="appearance-screen" aria-busy={status === 'loading'}>
	<h1 class="as-h1">{m.appearance_title()}</h1>
	<p class="as-text">{m.appearance_lead()}</p>
	{#if status === 'failed'}
		<Banner kind="error">{m.appearance_load_failed()}</Banner>
	{:else if status === 'ready'}
		{#if Object.keys(locks).length}<Banner kind="info">{m.appearance_locked()}</Banner>{/if}
		{#if missing}<Banner kind="info">{m.appearance_custom_missing()}</Banner>{/if}
		{#if failed}<Banner kind="error">{m.appearance_save_failed()}</Banner>{/if}
		<div role="status" class="as-status">
			{#if undo}
				{m.appearance_saved()}
				<Button
					variant="quiet"
					onclick={() => {
						const back = undo!;
						undo = null;
						void change(back).then(() => (undo = null));
					}}>{m.appearance_undo()}</Button
				>
			{/if}
		</div>

		<section class="as-section" aria-labelledby="ap-theme">
			<h2 id="ap-theme" class="as-h2">{m.appearance_theme()}</h2>
			<p class="as-text">{m.appearance_theme_help()}</p>
			<SegmentedControl
				label={m.appearance_mode()}
				options={modeOptions}
				value={prefs.mode}
				onchange={(v) => void change({ mode: v as Appearance['mode'] })}
			/>
			{#if picker}
				{@const Picker = picker}
				<Picker
					{prefs}
					dark={prefersDark(prefs.mode)}
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
				label={m.appearance_motion()}
				options={motionOptions}
				value={prefs.motion}
				onchange={(v) => void change({ motion: v as Appearance['motion'] })}
			/>
		</section>

		<section class="as-section">
			<Toggle
				label={m.appearance_illustrations()}
				checked={prefs.illustrations}
				onchange={(e) => void change({ illustrations: e.currentTarget.checked })}
			/>
			<p class="as-text">{m.appearance_illustrations_help()}</p>
		</section>

		<section class="as-section" aria-labelledby="ap-adult">
			<h2 id="ap-adult" class="as-h2">{m.appearance_adult()}</h2>
			<p class="as-text">{m.appearance_adult_help()}</p>
			{#if adultState === 'failed'}<Banner kind="error">{m.appearance_adult_failed()}</Banner>{/if}
			{#if adultState !== 'unknown'}
				<Toggle
					label={m.appearance_adult_label()}
					checked={adult || adultState === 'asking'}
					onchange={(e) => {
						if (!e.currentTarget.checked) void setAdult(false);
						else adultState = 'asking';
					}}
				/>
			{/if}
			{#if adultState === 'asking'}
				<div class="as-section" role="group" aria-label={m.appearance_adult_confirm_title()}>
					<Banner kind="warning">{m.appearance_adult_confirm()}</Banner>
					<div class="as-actions">
						<Button variant="primary" onclick={() => void setAdult(true)}>
							{m.appearance_adult_confirm_yes()}
						</Button>
						<Button onclick={() => (adultState = 'ready')}>{m.appearance_adult_confirm_no()}</Button
						>
					</div>
				</div>
			{/if}
		</section>
	{/if}
</div>
