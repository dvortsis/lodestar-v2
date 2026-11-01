<script lang="ts">
	// Advanced (§10.10.14): channel, wait after a release, install window, index rebuilds and what to
	// be notified about. Loaded when the section is first opened. Every control shows the server's
	// value: a refused or failed save puts it back and shows the error. Moving to a less stable
	// channel asks first; Cancel sends nothing.
	import { tick } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING } from '$lib/ui/classes';
	import Input from '$lib/ui/fields/Input.svelte';
	import { ask } from '../form/dialogs';
	import type { UpdatesStore } from './store.svelte';

	let { store }: { store: UpdatesStore } = $props();
	const values = $derived(store.settings?.values ?? {});
	const lock = (name: string) => store.settings?.locks[name];
	const lockText = (name: string) => {
		const l = lock(name);
		return l === 'read_only'
			? m.settings_updates_locked_read_only()
			: l
				? m.settings_updates_locked({ variable: l })
				: '';
	};

	let root = $state<HTMLElement>();
	let wait = $state('');
	let installWindow = $state('');
	let focused = $state(false);
	// The text fields start from, and go back to, the server's value.
	function reset() {
		wait = String(values.wait_days ?? '');
		installWindow = String(values.window ?? '');
	}
	$effect(() => {
		if (!focused) reset();
	});

	const failure: Record<string, () => string> = {
		setting: m.settings_updates_error_setting,
		locked: m.settings_updates_error_locked,
		stale: m.settings_updates_error_stale,
		busy: m.settings_updates_error_busy
	};
	const channels = ['stable', 'beta', 'nightly'] as const;
	type Channel = (typeof channels)[number];
	const channelText: Record<Channel, [() => string, () => string]> = {
		stable: [m.settings_updates_adv_channel_stable, m.settings_updates_adv_channel_stable_help],
		beta: [m.settings_updates_adv_channel_beta, m.settings_updates_adv_channel_beta_help],
		nightly: [m.settings_updates_adv_channel_nightly, m.settings_updates_adv_channel_nightly_help]
	};
	const channelWarn = {
		stable: m.settings_updates_adv_switch_stable,
		beta: m.settings_updates_adv_switch_beta,
		nightly: m.settings_updates_adv_switch_nightly
	};

	/** Puts the radios and toggles back on what the server last said. */
	async function sync() {
		await tick();
		if (!root) return;
		for (const i of root.querySelectorAll<HTMLInputElement>('input[data-field]')) {
			const v = values[i.dataset.field ?? ''];
			i.checked = i.type === 'radio' ? v === i.value : v === true;
		}
		reset();
	}

	// One question at a time: arrowing through the radios fires a change for each step, and a second
	// dialog opened over the first could save a channel nobody chose.
	let asking = false;
	const rank = { stable: 0, beta: 1, nightly: 2 } as const;
	async function pickChannel(to: Channel) {
		if (to === values.channel) return;
		if (asking) {
			await sync();
			return;
		}
		asking = true;
		// Moving to a more stable channel doesn't warn about snapshots; it never goes back a version.
		const from = values.channel as Channel;
		const warn =
			rank[to] < (rank[from] ?? 0) ? m.settings_updates_adv_switch_back : channelWarn[to];
		const answer = await ask(
			{
				title: m.settings_updates_adv_switch_title({ channel: channelText[to][0]() }),
				body: [
					warn({ channel: channelText[to][0]() }),
					// Moving back to Beta from Nightly still says what Beta means.
					...(to === 'beta' && rank[to] < (rank[from] ?? 0) ? [channelText.beta[1]()] : []),
					m.settings_updates_adv_switch_when()
				],
				actions: [
					{ id: 'cancel', label: m.settings_updates_cancel() },
					{ id: 'switch', label: m.settings_updates_adv_switch_action(), variant: 'primary' }
				],
				initial: 'cancel'
			},
			store.signal
		);
		asking = false;
		if (answer === 'switch') await store.set('channel', to);
		await sync();
	}

	async function save(name: string, value: unknown) {
		// An empty or non-whole number is not an answer: put the server's value back, send nothing.
		if (name === 'wait_days' && (value === '' || value === null || !Number.isInteger(value))) {
			await sync();
			return;
		}
		await store.set(name, value);
		await sync();
	}
	const notes = [
		['notify_before', m.settings_updates_adv_notify_before],
		['notify_after', m.settings_updates_adv_notify_after],
		['notify_failure', m.settings_updates_adv_notify_failure]
	] as const;
</script>

<div class="flex flex-col gap-4" bind:this={root} data-testid="updates-advanced">
	{#if store.error && failure[store.error]}
		<p role="alert" data-testid="updates-adv-reason">
			{store.reason || failure[store.error]?.()}
		</p>
	{/if}

	<fieldset class="flex flex-col gap-2" data-testid="updates-channel">
		<legend class="text-small font-semibold">{m.settings_updates_adv_channel()}</legend>
		{#if lock('channel')}<p class="text-small">{lockText('channel')}</p>{/if}
		{#each channels as c (c)}
			<label class="inline-flex items-center gap-2 text-body">
				<input
					type="radio"
					name="updates-channel"
					value={c}
					class="radio border-field-border {FOCUS_RING}"
					checked={values.channel === c}
					disabled={!!lock('channel')}
					aria-describedby="updates-channel-{c}-help"
					data-field="channel"
					data-testid="updates-channel-{c}"
					onchange={() => void pickChannel(c)}
				/>
				<span>{channelText[c][0]()}</span>
			</label>
			<p id="updates-channel-{c}-help" class="ps-4 text-small text-muted-text">
				{channelText[c][1]()}
			</p>
		{/each}
	</fieldset>

	<div class="flex flex-col gap-1">
		<label for="updates-adv-wait" class="font-semibold">{m.settings_updates_adv_wait()}</label>
		<Input
			id="updates-adv-wait"
			type="number"
			min="0"
			max="14"
			bind:value={wait}
			disabled={!!lock('wait_days')}
			aria-describedby="updates-adv-wait-help"
			data-testid="updates-adv-wait"
			onfocus={() => (focused = true)}
			onblur={() => (focused = false)}
			onchange={() => void save('wait_days', wait === '' || wait == null ? null : Number(wait))}
		/>
		<p id="updates-adv-wait-help" class="text-small text-muted-text">
			{m.settings_updates_adv_wait_help()}
			{lockText('wait_days')}
		</p>
	</div>

	<div class="flex flex-col gap-1">
		<label for="updates-adv-window" class="font-semibold">{m.settings_updates_adv_window()}</label>
		<Input
			id="updates-adv-window"
			bind:value={installWindow}
			disabled={!!lock('window')}
			aria-describedby="updates-adv-window-help"
			data-testid="updates-adv-window"
			onfocus={() => (focused = true)}
			onblur={() => (focused = false)}
			onchange={() => void save('window', installWindow)}
		/>
		<p id="updates-adv-window-help" class="text-small text-muted-text">
			{m.settings_updates_adv_window_help()}
			{lockText('window')}
		</p>
	</div>

	<div class="flex flex-col gap-1">
		<label class="inline-flex items-center gap-2 text-body">
			<input
				type="checkbox"
				role="switch"
				class="toggle border-field-border {FOCUS_RING}"
				checked={values.allow_reindex === true}
				disabled={!!lock('allow_reindex')}
				aria-describedby="updates-adv-reindex-help"
				data-field="allow_reindex"
				data-testid="updates-adv-reindex"
				onchange={(e) => void save('allow_reindex', e.currentTarget.checked)}
			/>
			<span>{m.settings_updates_adv_reindex()}</span>
		</label>
		<p id="updates-adv-reindex-help" class="ps-4 text-small text-muted-text">
			{m.settings_updates_adv_reindex_help()}
			{lockText('allow_reindex')}
		</p>
	</div>

	<fieldset class="flex flex-col gap-2">
		<legend class="text-small font-semibold">{m.settings_updates_adv_notify()}</legend>
		<p class="text-small text-muted-text">{m.settings_updates_adv_notify_help()}</p>
		{#each notes as [name, label] (name)}
			<label class="inline-flex items-center gap-2 text-body">
				<input
					type="checkbox"
					role="switch"
					class="toggle border-field-border {FOCUS_RING}"
					checked={values[name] === true}
					disabled={!!lock(name)}
					data-field={name}
					data-testid="updates-adv-{name}"
					onchange={(e) => void save(name, e.currentTarget.checked)}
				/>
				<span>{label()}</span>
			</label>
			{#if lock(name)}<p class="ps-4 text-small">{lockText(name)}</p>{/if}
		{/each}
	</fieldset>
</div>
