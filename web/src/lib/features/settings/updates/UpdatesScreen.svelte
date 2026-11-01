<script lang="ts">
	// Settings → Updates (§10.10.14, part 1). It shows only what the server says (`GET /updates`
	// and the `updates` settings): a failed or refused request leaves the last server state on
	// screen with an error, and a control's position is put back to the server's value afterwards.
	// Release notes and history load right after the first paint (Releases.svelte). Roll back and the restart overlay are part 2 (docs/decisions/*-S-updates.md).
	import { onDestroy, onMount, tick, type Component } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { FOCUS_RING } from '$lib/ui/classes';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { whenOf } from './time';
	import { ask } from '../form/dialogs';
	import {
		LEVELS,
		PAUSE_DAYS,
		cannotAuto,
		newest,
		noticesOf,
		securityValue,
		securityVisible,
		STEPS,
		type Level,
		type Mode
	} from './logic';
	import { UpdatesStore } from './store.svelte';

	const store = new UpdatesStore();
	onMount(() => store.start());
	onDestroy(() => store.stop());

	const status = $derived(store.status);
	const settings = $derived(store.settings);
	const mode = $derived<Mode>(settings?.mode ?? 'off');
	const noEngine = $derived(store.unavailable || !!status?.unavailable);
	const noAuto = $derived(noEngine ? (status?.unavailable ?? 'unavailable') : cannotAuto(status));
	const release = $derived(status ? newest(status) : undefined);
	const canInstall = $derived(
		!!release &&
			!store.step &&
			!status?.installing &&
			status?.unavailable === null &&
			!!status?.capability.can_install
	);
	const rolledBack = $derived(status?.history.findLast((h) => h.result === 'rolled_back'));
	/** A time as the server gave it, in words that carry their own date. */
	const when = (seconds: number) => whenOf(seconds, currentLocale());
	const lock = (name: string) => settings?.locks[name];
	const lockText = (name: string) => {
		const l = lock(name);
		return l === 'read_only'
			? m.settings_updates_locked_read_only()
			: l
				? m.settings_updates_locked({ variable: l })
				: '';
	};

	let form = $state<HTMLElement>();
	/** Puts every control back on the server's value, whether the change was saved or refused. */
	async function sync() {
		await tick();
		if (!form || !settings) return;
		for (const i of form.querySelectorAll<HTMLInputElement>('input[data-field]')) {
			const f = i.dataset.field;
			i.checked =
				f === 'security_fixes'
					? securityValue(status, mode, settings?.securityFixes)
					: f === 'mode'
						? settings.mode === i.value
						: settings.level === i.value;
		}
	}
	async function change(name: 'mode' | 'level' | 'security_fixes', value: string) {
		await store.set(name, value);
		await sync();
	}

	const causes: Record<string, () => string> = {
		not_supervised: m.settings_updates_cannot_not_supervised,
		dev_build: m.settings_updates_cannot_dev_build,
		noexec: m.settings_updates_cannot_noexec,
		not_writable: m.settings_updates_cannot_not_writable,
		package_managed: m.settings_updates_cannot_package_managed,
		image_mode: m.settings_updates_cannot_image_mode,
		trust_damaged: m.settings_updates_cannot_trust_damaged,
		channel_unavailable: m.settings_updates_cannot_channel_unavailable,
		not_built_yet: m.settings_updates_cannot_not_built_yet
	};
	const cause = (r: string) => causes[r]?.() ?? m.settings_updates_cannot_other({ reason: r });

	const notice: Record<string, [() => string, () => string]> = {
		revoked_key: [
			m.settings_updates_notice_revoked_key,
			m.settings_updates_notice_revoked_key_body
		],
		trust_damaged: [
			m.settings_updates_notice_trust_damaged,
			m.settings_updates_notice_trust_damaged_body
		],
		clock_behind: [
			m.settings_updates_notice_clock_behind,
			m.settings_updates_notice_clock_behind_body
		],
		clock_ahead: [
			m.settings_updates_notice_clock_ahead,
			m.settings_updates_notice_clock_ahead_body
		],
		updater_unreadable: [
			m.settings_updates_notice_updater_unreadable,
			m.settings_updates_notice_updater_unreadable_body
		],
		held_in_process: [
			m.settings_updates_notice_held_in_process,
			m.settings_updates_notice_held_in_process_body
		],
		check_not_saved: [
			m.settings_updates_notice_check_not_saved,
			m.settings_updates_notice_check_not_saved_body
		],
		pending_unwritable: [
			m.settings_updates_notice_pending_unwritable,
			m.settings_updates_notice_pending_unwritable_body
		],
		bad_overlay: [
			m.settings_updates_notice_bad_overlay,
			m.settings_updates_notice_bad_overlay_body
		],
		manifest_lapsed: [
			m.settings_updates_notice_manifest_lapsed,
			m.settings_updates_notice_manifest_lapsed_body
		],
		channel_unavailable: [
			m.settings_updates_notice_channel_unavailable,
			m.settings_updates_notice_channel_unavailable_body
		]
	};

	const modes: { id: Mode; label: () => string; help: () => string }[] = [
		{ id: 'off', label: m.settings_updates_mode_off, help: m.settings_updates_mode_off_help },
		{
			id: 'notify',
			label: m.settings_updates_mode_notify,
			help: m.settings_updates_mode_notify_help
		},
		{ id: 'auto', label: m.settings_updates_mode_auto, help: m.settings_updates_mode_auto_help },
		{ id: 'image', label: m.settings_updates_mode_image, help: m.settings_updates_mode_image_help }
	];
	const levels: Record<Level, [() => string, () => string]> = {
		fixes: [m.settings_updates_level_fixes, m.settings_updates_level_fixes_help],
		features: [m.settings_updates_level_features, m.settings_updates_level_features_help],
		all: [m.settings_updates_level_all, m.settings_updates_level_all_help]
	};
	const pauseLabel: Record<number, () => string> = {
		1: m.settings_updates_pause_1,
		7: m.settings_updates_pause_7,
		30: m.settings_updates_pause_30
	};
	const steps: Record<string, () => string> = {
		downloading: m.settings_updates_step_downloading,
		verifying: m.settings_updates_step_verifying,
		installing: m.settings_updates_step_installing,
		restarting: m.settings_updates_step_restarting,
		checking_health: m.settings_updates_step_checking_health
	};
	const errors: Record<string, () => string> = {
		load: m.settings_updates_error_load,
		check: m.settings_updates_error_check,
		pause: m.settings_updates_error_pause,
		skip: m.settings_updates_error_skip,
		clear: m.settings_updates_error_clear,
		setting: m.settings_updates_error_setting,
		locked: m.settings_updates_error_locked,
		stale: m.settings_updates_error_stale,
		busy: m.settings_updates_error_busy,
		install: m.settings_updates_error_install,
		install_unknown_version: m.settings_updates_error_install_unknown_version,
		install_version_skipped: m.settings_updates_error_install_version_skipped,
		install_manual_confirmation: m.settings_updates_error_install_manual_confirmation,
		install_not_saved: m.settings_updates_error_install_not_saved,
		rollback: m.settings_updates_error_rollback,
		rollback_updates_unavailable: m.settings_updates_error_rollback_updates_unavailable
	};
	const rollbackReasons: Record<string, () => string> = {
		not_supervised: m.settings_updates_rollback_reason_not_supervised,
		image_mode: m.settings_updates_rollback_reason_image_mode,
		busy: m.settings_updates_rollback_reason_busy,
		no_previous_version: m.settings_updates_rollback_reason_no_previous_version,
		snapshot_missing: m.settings_updates_rollback_reason_snapshot_missing
	};
	/** A plain sentence per stable install-failure code; never the code's own text, never a URL. */
	const failedWords: Record<string, () => string> = {
		download_mismatch: m.settings_updates_failed_download_mismatch,
		download_too_large: m.settings_updates_failed_download_too_large,
		download_io: m.settings_updates_failed_download_io,
		no_artifact: m.settings_updates_failed_no_artifact,
		redirect_refused: m.settings_updates_failed_redirect_refused,
		stage_failed: m.settings_updates_failed_stage_failed
	};
	const installReasons: Record<string, () => string> = {
		busy: m.settings_updates_install_reason_busy,
		not_candidate: m.settings_updates_install_reason_not_candidate,
		not_current: m.settings_updates_install_reason_not_current,
		upgrade_path: m.settings_updates_install_reason_upgrade_path
	};
	/** The words for the last refusal; `install_unavailable` adds its reason. */
	const errorText = (kind: string) =>
		kind === 'install_unavailable'
			? m.settings_updates_error_install_unavailable({
					reason: store.reason
						? (installReasons[store.reason]?.() ?? cause(store.reason))
						: m.settings_updates_install_reason_unknown()
				})
			: kind === 'rollback_unavailable'
				? m.settings_updates_error_rollback_unavailable({
						reason:
							(store.reason && rollbackReasons[store.reason]?.()) ||
							m.settings_updates_rollback_reason_unknown()
					})
				: kind === 'install_failed'
					? m.settings_updates_error_install_failed({
							reason:
								(store.reason && failedWords[store.reason]?.()) || m.settings_updates_failed_other()
						})
					: errors[kind]?.();

	/** Install now: a release with manual steps asks first; only a confirm sends `confirm_manual`. */
	async function install(r: { version: string; manual_steps: boolean }) {
		if (r.manual_steps) {
			const answer = await ask(
				{
					title: m.settings_updates_install_title({ version: r.version }),
					body: [m.settings_updates_install_manual_body()],
					actions: [
						{ id: 'cancel', label: m.settings_updates_cancel() },
						{ id: 'install', label: m.settings_updates_install_action(), variant: 'primary' }
					],
					initial: 'cancel'
				},
				store.signal
			);
			if (answer !== 'install') return;
			await store.install(r.version, true);
		} else await store.install(r.version);
	}

	/** The confirmation: Cancel and Esc send nothing. */
	async function skip(version: string) {
		const answer = await ask(
			{
				title: m.settings_updates_skip_title({ version }),
				body: [m.settings_updates_skip_body()],
				actions: [
					{ id: 'cancel', label: m.settings_updates_cancel() },
					{ id: 'skip', label: m.settings_updates_skip(), variant: 'primary' }
				],
				initial: 'cancel'
			},
			store.signal
		);
		if (answer === 'skip') await store.skip(version);
	}

	/** Roll back: the confirmation says what is lost; only Roll back sends `confirm: true`. */
	async function rollBack(to: string) {
		const lost = status?.rollback.lost_torrents;
		const answer = await ask(
			{
				title: m.settings_updates_rollback_title({ version: to }),
				body: [
					m.settings_updates_rollback_body({ version: to }),
					lost != null && lost > 0
						? m.settings_updates_rollback_lost({ count: lost })
						: m.settings_updates_rollback_lost_unknown()
				],
				actions: [
					{ id: 'cancel', label: m.settings_updates_cancel() },
					{ id: 'rollback', label: m.settings_updates_rollback_action(), variant: 'danger' }
				],
				initial: 'cancel'
			},
			store.signal
		);
		if (answer === 'rollback') await store.rollback();
	}

	// Advanced loads when it is first opened.
	let Advanced = $state<Component<{ store: UpdatesStore }> | false>();
	function openAdvanced(e: Event) {
		if (!(e.currentTarget as HTMLDetailsElement).open || Advanced) return;
		import('./Advanced.svelte').then(
			(p) => (Advanced = p.default),
			() => (Advanced = false)
		);
	}

	let Parts = $state<Component<{ store: UpdatesStore }> | false>();
	function loadParts() {
		Parts = undefined;
		import('./Releases.svelte').then(
			(p) => (Parts = p.default),
			() => (Parts = false)
		);
	}
	onMount(loadParts);
</script>

{#snippet revokedInstall()}
	<Button
		variant="primary"
		onclick={() => release && install(release)}
		loading={store.busy === 'install'}
		data-testid="updates-install-revoked">{m.settings_updates_install_now()}</Button
	>
{/snippet}

{#snippet choice(o: {
	field: 'mode' | 'level';
	value: string;
	label: string;
	checked: boolean;
	disabled: boolean;
	onchange: () => void;
})}
	<label
		class="inline-flex cursor-pointer items-center gap-2 text-body pointer-coarse:min-h-(--target-min)"
	>
		<input
			type="radio"
			name="updates-{o.field}"
			value={o.value}
			class="radio border-field-border {FOCUS_RING}"
			checked={o.checked}
			disabled={o.disabled}
			aria-describedby="updates-{o.field}-{o.value}-help"
			data-field={o.field}
			data-testid="updates-{o.field}-{o.value}"
			onchange={o.onchange}
		/>
		<span>{o.label}</span>
	</label>
{/snippet}

<h1 class="text-title font-semibold">{m.settings_updates_title()}</h1>
<p class="text-muted-text">{m.settings_updates_purpose()}</p>

{#if store.loading && status === undefined && settings === undefined}
	<p role="status" aria-busy="true" class="sr-only">{m.settings_updates_loading()}</p>
	<Skeleton lines={4} />
{:else if status === undefined && settings === undefined}
	<Banner kind="error">
		{store.unavailable
			? m.settings_updates_unavailable({ reason: '—' })
			: m.settings_updates_load_failed()}
		{#snippet action()}
			<Button onclick={() => store.load()}>{m.settings_updates_reload()}</Button>
		{/snippet}
	</Banner>
{:else}
	<div class="flex flex-col gap-8">
		{#if store.error}
			<Banner kind="error"><span data-testid="updates-error">{errorText(store.error)}</span></Banner
			>
		{/if}
		{#if noEngine}
			<Banner kind="info"
				><span data-testid="updates-unavailable"
					>{m.settings_updates_unavailable({ reason: status?.unavailable ?? '—' })}</span
				></Banner
			>
		{/if}

		{#if status}
			<div class="flex flex-col gap-3" data-testid="updates-notices">
				{#each noticesOf(status) as code (code)}
					<Banner
						kind="warning"
						title={notice[code]?.[0]() ?? m.settings_updates_notice_unknown({ code })}
						action={code === 'revoked_key' && canInstall ? revokedInstall : undefined}
					>
						<span data-testid="updates-notice-{code}"
							>{notice[code]?.[1]() ?? m.settings_updates_notice_unknown_body()}</span
						>
					</Banner>
				{/each}
			</div>

			<section class="flex flex-col gap-3" aria-labelledby="updates-status">
				<h2 id="updates-status" class="text-label font-semibold">
					{m.settings_updates_status_heading()}
				</h2>
				{#if store.step}
					{@const stepVersion = store.stepVersion ?? status.pending?.version}
					<div data-testid="updates-progress" role="status">
						<p class="font-semibold">
							{stepVersion
								? m.settings_updates_progress_heading({ version: stepVersion })
								: m.settings_updates_progress_heading_unversioned()}
						</p>
						<ol class="flex flex-wrap gap-x-4 gap-y-1">
							{#each STEPS as s (s)}
								<li
									aria-current={s === store.step ? 'step' : undefined}
									class={s === store.step ? 'font-semibold' : 'text-muted-text'}
								>
									{steps[s]?.()}{s === store.step ? ` (${m.settings_updates_step_current()})` : ''}
								</li>
							{/each}
						</ol>
					</div>
				{:else if release}
					<p data-testid="updates-available" class="font-semibold">
						{m.settings_updates_available({ version: release.version })}
					</p>
					<p class="text-muted-text" data-testid="updates-when">
						{#if release.installs_at}
							{m.settings_updates_installs_at({ when: when(release.installs_at) })}
						{:else}
							{m.settings_updates_waits()}
						{/if}
					</p>
				{:else if status.status.state === 'checking'}
					<p role="status">{m.settings_updates_checking()}</p>
				{:else if status.status.state === 'failed'}
					<p data-testid="updates-state">
						{m.settings_updates_check_failed({
							version: status.running.version,
							code: status.status.error ?? '—'
						})}
					</p>
				{:else if status.status.state === 'ok' && status.status.checked_at}
					<p data-testid="updates-state">
						{m.settings_updates_current({
							version: status.running.version,
							when: when(status.status.checked_at)
						})}
					</p>
				{:else}
					<p data-testid="updates-state">
						{m.settings_updates_not_checked({ version: status.running.version })}
					</p>
				{/if}

				{#if rolledBack && status.notices.includes('rolled_back')}
					<Banner kind="warning">
						<span data-testid="updates-rolled-back">
							{m.settings_updates_rolled_back({
								version: rolledBack.version,
								reason: rolledBack.reason ?? ''
							})}
							{m.settings_updates_rolled_back_next()}
						</span>
					</Banner>
				{/if}

				<div class="flex flex-wrap items-center gap-2">
					<Button
						onclick={() => store.check()}
						loading={store.busy === 'check'}
						disabled={status.unavailable !== null}
						data-testid="updates-check">{m.settings_updates_check_now()}</Button
					>
					<!-- Install now: hidden whenever the capability says no (trust damaged included), while
					     an install is under way, or when the engine is unavailable. -->
					{#if release && !store.step && status.unavailable === null}
						{#if status.capability.can_install && !status.installing}
							<Button
								variant="primary"
								onclick={() => install(release)}
								loading={store.busy === 'install'}
								data-testid="updates-install">{m.settings_updates_install_now()}</Button
							>
						{/if}
						<Button
							onclick={() => skip(release.version)}
							loading={store.busy === 'skip'}
							data-testid="updates-skip">{m.settings_updates_skip()}</Button
						>
					{/if}
					{#if status.rollback.available && status.rollback.to_version && !store.step}
						{@const to = status.rollback.to_version}
						<Button
							onclick={() => rollBack(to)}
							loading={store.busy === 'rollback'}
							data-testid="updates-rollback"
							>{m.settings_updates_rollback_button({
								version: to === 'image' ? m.settings_updates_rollback_image() : to
							})}</Button
						>
					{/if}
				</div>
			</section>
		{/if}

		{#if settings}
			<section class="flex flex-col gap-3" aria-labelledby="updates-how" bind:this={form}>
				<h2 id="updates-how" class="text-label font-semibold">
					{m.settings_updates_how_heading()}
				</h2>
				<p class="text-muted-text" data-testid="updates-contacts">
					{m.settings_updates_contacts()}
				</p>
				{#if lock('mode')}
					<p class="text-small" data-testid="updates-locked-mode">{lockText('mode')}</p>
				{/if}
				<div role="radiogroup" aria-labelledby="updates-how" class="flex flex-col gap-3">
					{#each modes as o (o.id)}
						{#if o.id !== 'image' || store.imageOffered || mode === 'image'}
							{@const blocked = o.id === 'auto' && !!noAuto && mode !== 'auto'}
							<div class="flex flex-col gap-1">
								{@render choice({
									field: 'mode',
									value: o.id,
									label: o.label(),
									checked: mode === o.id,
									disabled: !!lock('mode') || blocked,
									onchange: () => void change('mode', o.id)
								})}
								<p id="updates-mode-{o.id}-help" class="ps-4 text-small text-muted-text">
									{o.help()}
									{#if o.id === 'auto' && noAuto}
										<span data-testid="updates-cannot-auto"
											>{m.settings_updates_cannot_auto({ reason: cause(noAuto) })}</span
										>
									{/if}
								</p>
							</div>
							{#if o.id === 'auto' && mode === 'auto'}
								<fieldset class="ps-4 flex flex-col gap-2" data-testid="updates-levels">
									<legend class="text-label font-semibold"
										>{m.settings_updates_level_heading()}</legend
									>
									{#if lock('level')}<p class="text-small">{lockText('level')}</p>{/if}
									{#each LEVELS as l (l)}
										{@render choice({
											field: 'level',
											value: l,
											label: levels[l][0](),
											checked: settings.level === l,
											disabled: !!lock('level'),
											onchange: () => void change('level', l)
										})}
										<p id="updates-level-{l}-help" class="ps-4 text-small text-muted-text">
											{levels[l][1]()}
										</p>
									{/each}
								</fieldset>
							{/if}
						{/if}
					{/each}
				</div>

				{#if securityVisible(mode)}
					<div class="flex flex-col gap-1" data-testid="updates-security">
						<label
							class="inline-flex cursor-pointer items-center gap-2 text-body pointer-coarse:min-h-(--target-min)"
						>
							<input
								type="checkbox"
								role="switch"
								class="toggle border-field-border {FOCUS_RING}"
								checked={securityValue(status, mode, settings?.securityFixes)}
								disabled={!!lock('security_fixes')}
								aria-describedby="updates-security-help"
								data-field="security_fixes"
								data-testid="updates-security-toggle"
								onchange={(e) =>
									void change('security_fixes', e.currentTarget.checked ? 'on' : 'off')}
							/>
							<span>{m.settings_updates_security_label()}</span>
						</label>
						<p id="updates-security-help" class="ps-4 text-small text-muted-text">
							{mode === 'auto'
								? m.settings_updates_security_help_auto()
								: m.settings_updates_security_help_notify()}
							{lockText('security_fixes')}
						</p>
					</div>
				{/if}
			</section>
		{/if}

		{#if status && (mode === 'auto' || mode === 'notify')}
			<section
				class="flex flex-col gap-2"
				aria-labelledby="updates-pause"
				data-testid="updates-pause"
			>
				<h2 id="updates-pause" class="text-label font-semibold">
					{m.settings_updates_pause_heading()}
				</h2>
				{#if status.policy.paused_until}
					<p data-testid="updates-paused">
						{m.settings_updates_paused({ when: when(status.policy.paused_until) })}
					</p>
					<div>
						<Button
							onclick={() => store.pause(0)}
							loading={store.busy === 'pause'}
							data-testid="updates-resume">{m.settings_updates_resume()}</Button
						>
					</div>
				{:else}
					<div class="flex flex-wrap gap-2">
						{#each PAUSE_DAYS as d (d)}
							<Button
								onclick={() => store.pause(d)}
								loading={store.busy === 'pause'}
								data-testid="updates-pause-{d}">{pauseLabel[d]?.()}</Button
							>
						{/each}
					</div>
				{/if}
			</section>
		{/if}

		{#if settings}
			<details ontoggle={openAdvanced} data-testid="updates-advanced-section">
				<summary class="cursor-pointer text-label font-semibold"
					>{m.settings_updates_adv_heading()}</summary
				>
				{#if Advanced}
					<Advanced {store} />
				{:else if Advanced === false}
					<p>{m.settings_updates_part_failed()}</p>
				{/if}
			</details>
		{/if}

		{#if Parts}
			<Parts {store} />
		{:else if Parts === false}
			<p class="flex flex-wrap items-center gap-3">
				{m.settings_updates_part_failed()}
				<Button onclick={loadParts} data-testid="updates-parts-retry"
					>{m.settings_updates_retry()}</Button
				>
			</p>
		{/if}
	</div>
{/if}
