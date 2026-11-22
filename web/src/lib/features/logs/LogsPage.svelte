<script lang="ts">
	import { PAGE_BODY_FULL, PAGE_FRAME, PAGE_H1 } from '$lib/ui/layout/classes';
	// The live log (§10.10.12): a full-width, virtualised list of one-line rows that follows
	// `GET /logs/stream`. All text is untrusted and goes through `safe()` into text nodes only.
	import { displayTimeZone } from '$lib/i18n/locale.svelte';
	import { onMount, tick } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { resolve } from '$app/paths';
	import IconError from '~icons/ph/x-circle-bold';
	import IconWarn from '~icons/ph/warning-bold';
	import IconInfo from '~icons/ph/info-bold';
	import IconCheck from '~icons/ph/check-circle-bold';
	import IconDebug from '~icons/ph/bug-bold';
	import IconTrace from '~icons/ph/dots-three-circle-bold';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import VirtualList from '$lib/ui/lists/VirtualList.svelte';
	import { createLogView, MAX_LINES, NO_FILTERS } from './logs.svelte';
	import { getLoggingState, remaining, type LoggingState } from './status';
	import {
		FIELD_MAX,
		formatTime,
		lineKey,
		lineText,
		safe,
		type LogComponent,
		type LogLevel,
		type LogLine
	} from './text';

	const view = createLogView();
	const COMPONENTS: LogComponent[] = [
		'system',
		'dht',
		'fetch',
		'pipeline',
		'search',
		'api',
		'sources',
		'filters',
		'integrations',
		'workers',
		'companions',
		'auth',
		'libraries'
	];
	const LEVELS: LogLevel[] = ['error', 'warn', 'info', 'debug', 'trace'];
	const levelName = (l: LogLevel) =>
		({
			error: m.logs_level_error,
			warn: m.logs_level_warn,
			info: m.logs_level_info,
			debug: m.logs_level_debug,
			trace: m.logs_level_trace
		})[l]();
	const ICONS = {
		error: IconError,
		warn: IconWarn,
		info: IconInfo,
		debug: IconDebug,
		trace: IconTrace
	};
	const TONE = {
		error: 'text-error',
		warn: 'text-warning',
		info: '',
		debug: 'text-muted-text',
		trace: 'text-muted-text'
	};

	let level = $state<LogLevel>(NO_FILTERS.level);
	let components = $state<LogComponent[]>([]);
	let text = $state('');
	let utc = $state(false);
	// `null` is "not chosen yet": the row's own CSS decides (wrapped below 640 px, one line above), so
	// the list never switches layout after the first paint. A press of the toggle fixes it either way.
	let wrap = $state<boolean | null>(null);
	const NARROW = '(max-width: 39.99rem)';
	let narrow = $state(typeof matchMedia === 'function' && matchMedia(NARROW).matches);
	onMount(() => {
		if (typeof matchMedia !== 'function') return;
		const mq = matchMedia(NARROW);
		const on = () => (narrow = mq.matches);
		on();
		mq.addEventListener('change', on);
		return () => mq.removeEventListener('change', on);
	});
	const wrapped = $derived(wrap ?? narrow);
	// A phone row is two lines (time, level, component, then the message), about 44 px: the first
	// paint then matches the measured height and the list doesn't move once rows are measured.
	const ROW_TWO_LINES = 44;
	let root = $state<HTMLElement>();
	let wrapper = $state<HTMLElement>();
	let activeIndex = $state(0);
	let notice = $state('');
	let logging = $state<LoggingState | undefined>();
	let now = $state(Date.now());
	const expanded = new SvelteSet<string>();

	// The server filters: a change restarts the list (text after a short pause).
	let applied = $state({ level: 'trace', components: [] as string[], text: '' });
	$effect(() => {
		const next = { level, components: [...components], text: text.trim() };
		const t = setTimeout(() => {
			if (JSON.stringify(next) === JSON.stringify(applied)) return;
			applied = next;
			view.setFilters(next);
		}, 300);
		return () => clearTimeout(t);
	});

	onMount(() => {
		view.start();
		const ticker = setInterval(() => (now = Date.now()), 30_000);
		const ctl = new AbortController();
		void getLoggingState(ctl.signal).then((s) => (logging = s));
		return () => {
			view.stop();
			clearInterval(ticker);
			ctl.abort();
		};
	});

	const filtered = $derived(level !== 'trace' || components.length > 0 || text.trim() !== '');
	const scroller = () => wrapper?.querySelector<HTMLElement>('[role="listbox"]') ?? null;
	const atBottom = (el: HTMLElement) => el.scrollHeight - el.scrollTop - el.clientHeight < 8;

	// Following: stay at the bottom while on; scrolling up turns it off, reaching the bottom on.
	let lastTop = 0;
	function onScroll() {
		const el = scroller();
		if (!el) return;
		if (atBottom(el)) view.setFollowing(true);
		else if (el.scrollTop < lastTop) view.setFollowing(false);
		lastTop = el.scrollTop;
	}
	function toBottom() {
		const el = scroller();
		if (el) {
			el.scrollTop = el.scrollHeight;
			lastTop = el.scrollTop;
		}
	}
	$effect(() => {
		void view.lines;
		void view.generation;
		if (view.following && !view.paused) void tick().then(() => requestAnimationFrame(toBottom));
	});
	function jump() {
		view.setFollowing(true);
		if (view.paused) view.setPaused(false);
		void tick().then(toBottom);
	}

	function say(s: string) {
		notice = s;
		setTimeout(() => notice === s && (notice = ''), 4000);
	}
	const fieldsText = (l: LogLine) =>
		l.fields.map((f) => `${safe(f.name, 64)}=${safe(f.value, FIELD_MAX)}`).join(' ');
	const dump = () =>
		view.lines
			.map((l) =>
				[lineText(l, utc, FIELD_MAX, displayTimeZone()), fieldsText(l)].filter(Boolean).join(' ')
			)
			.join('\n');
	async function copy() {
		try {
			await navigator.clipboard.writeText(dump());
			say(m.logs_copied({ count: view.lines.length }));
		} catch {
			say(m.logs_copy_failed());
		}
	}
	function download() {
		const url = URL.createObjectURL(
			new Blob([dump() + '\n'], { type: 'text/plain;charset=utf-8' })
		);
		const a = document.createElement('a');
		a.href = url;
		a.download = 'lodestar-log-visible.txt';
		a.click();
		setTimeout(() => URL.revokeObjectURL(url), 1000);
	}

	function toggle(id: string) {
		if (!expanded.delete(id)) expanded.add(id);
	}
	const keyOf = (l: LogLine) => lineKey(l);
	function clearFilters() {
		level = 'trace';
		components = [];
		text = '';
	}
	function toggleComponent(c: LogComponent) {
		components = components.includes(c) ? components.filter((x) => x !== c) : [...components, c];
	}

	function onWindowKey(e: KeyboardEvent) {
		if (e.ctrlKey || e.metaKey || e.altKey) return;
		const t = e.target as HTMLElement;
		const editing = t.matches('input,textarea,select,[contenteditable]');
		if (e.key === '/' && !editing) {
			e.preventDefault();
			root?.querySelector<HTMLInputElement>('[data-logs-filter]')?.focus();
		} else if (e.key === ' ' && !editing && !t.closest('button,a,summary')) {
			e.preventDefault();
			view.setPaused(!view.paused);
		}
	}

	// Turn off now (§10.10.12): the same request as Settings → Logging's, loaded on the click. It
	// turns off everything this banner covers, then reads the server's state again.
	let turningOff = $state(false);
	let turnOffError = $state('');
	async function turnOff() {
		turningOff = true;
		turnOffError = '';
		try {
			const [{ LoggingCatalog }, { turnOffAll }] = await Promise.all([
				import('$lib/features/settings/logging/catalog.svelte'),
				import('$lib/features/settings/logging/privacy')
			]);
			await turnOffAll({ catalog: new LoggingCatalog() });
		} catch (e) {
			turnOffError = m.settings_logging_failed_off({ message: (e as Error).message });
		} finally {
			turningOff = false;
			logging = (await getLoggingState()) ?? logging;
		}
	}

	const loggingHref = $derived(resolve('/settings/[section]', { section: 'logging' }));
	const banner = $derived(logging ? remaining(logging, now) : null);
	const detailed = $derived(level === 'debug' || level === 'trace');
	const showStatus = $derived(
		view.status === 'live'
			? m.logs_status_live()
			: view.status === 'connecting'
				? m.logs_status_connecting()
				: view.status === 'reconnecting'
					? m.logs_status_reconnecting()
					: view.status === 'offline'
						? m.logs_status_offline()
						: m.logs_status_unavailable()
	);
</script>

<svelte:window onkeydown={onWindowKey} />

<div class={PAGE_FRAME} data-page-frame="">
	<section class={PAGE_BODY_FULL} aria-labelledby="logs-h">
		<div bind:this={root} class="logs flex flex-col gap-2">
			<div class="flex flex-wrap items-center gap-x-4 gap-y-1">
				<h1 id="logs-h" class={PAGE_H1} tabindex="-1">{m.logs_title()}</h1>
				<span class="text-small text-muted-text" data-testid="logs-status">{showStatus}</span>
				{#if logging}
					<a class="text-small underline" href={loggingHref}>{logging.chip}</a>
				{/if}
				<a class="ms-auto text-small underline" href={loggingHref}>{m.logs_open_full()}</a>
			</div>

			{#if banner}
				<Banner kind="warning" title={m.logs_identifying_title()}>
					{m.logs_identifying({ time: banner })}
					<a class="underline" href={loggingHref}>{m.logs_identifying_manage()}</a>
					{#if turnOffError}<p role="alert" class="text-small text-error">{turnOffError}</p>{/if}
					{#snippet action()}
						<Button variant="secondary" disabled={turningOff} onclick={turnOff}
							>{m.settings_logging_turn_off()}</Button
						>
					{/snippet}
				</Banner>
			{/if}

			<div
				class="logs-toolbar flex flex-wrap items-center gap-2"
				role="toolbar"
				aria-label={m.logs_toolbar()}
			>
				<label class="logs-field">
					<span class="text-caption text-muted-text">{m.logs_level_label()}</span>
					<Select
						dense
						bind:value={level}
						options={LEVELS.map((l) => ({
							value: l,
							label: m.logs_level_min({ level: levelName(l) })
						}))}
					/>
				</label>
				<label class="logs-field">
					<span class="text-caption text-muted-text">{m.logs_filter_label()}</span>
					<Input
						dense
						type="search"
						class="logs-filter"
						bind:value={text}
						data-logs-filter
						placeholder={m.logs_filter_placeholder()}
						maxlength={200}
					/>
				</label>
				<Button
					dense
					variant={utc ? 'primary' : 'secondary'}
					aria-pressed={utc}
					onclick={() => (utc = !utc)}
				>
					{utc ? m.logs_time_utc() : m.logs_time_local()}
				</Button>
				<Button
					dense
					variant={wrapped ? 'primary' : 'secondary'}
					aria-pressed={wrapped}
					onclick={() => (wrap = !wrapped)}
				>
					{m.logs_wrap()}
				</Button>
				<Button
					dense
					variant={view.paused ? 'primary' : 'secondary'}
					aria-pressed={view.paused}
					onclick={() => view.setPaused(!view.paused)}
				>
					{view.paused ? m.logs_resume() : m.logs_pause()}
				</Button>
				<Button
					dense
					disabled={!view.hasEarlier}
					loading={view.loadingEarlier}
					onclick={() => view.loadEarlier()}
				>
					{m.logs_load_earlier()}
				</Button>
				<Button dense disabled={!view.lines.length} onclick={copy}>{m.logs_copy()}</Button>
				<Button dense disabled={!view.lines.length} onclick={download}>{m.logs_download()}</Button>
			</div>
			<div class="flex flex-wrap gap-1" role="group" aria-label={m.logs_components()}>
				{#each COMPONENTS as c (c)}
					<Button
						dense
						variant={components.includes(c) ? 'primary' : 'secondary'}
						aria-pressed={components.includes(c)}
						onclick={() => toggleComponent(c)}
						>{#if components.includes(c)}<IconCheck aria-hidden="true" />{/if}{c}</Button
					>
				{/each}
			</div>

			{#if detailed && logging && !logging.recordsDebug}
				<Banner kind="info">{m.logs_not_recorded()}</Banner>
			{/if}
			{#if view.status === 'offline' || view.status === 'unavailable' || view.status === 'reconnecting'}
				<Banner kind={view.status === 'reconnecting' ? 'info' : 'warning'}>
					{view.status === 'offline'
						? m.logs_offline()
						: view.status === 'unavailable'
							? m.logs_stream_error()
							: m.logs_reconnecting()}
					<Button dense variant="quiet" onclick={() => view.retry()}>{m.logs_retry()}</Button>
				</Banner>
			{/if}
			{#if view.loadError}
				<Banner kind="error"
					>{m.logs_history_error()}
					<Button dense variant="quiet" onclick={() => view.retry()}>{m.logs_retry()}</Button>
				</Banner>
			{/if}
			{#if view.trimmed > 0}
				<p class="text-small text-muted-text" data-testid="logs-trimmed">
					{m.logs_trimmed({ count: view.trimmed, max: MAX_LINES })}
				</p>
			{/if}
			{#if view.skipped > 0}
				<p class="text-small text-muted-text">{m.logs_skipped({ count: view.skipped })}</p>
			{/if}

			<div
				class="relative min-h-0 flex-1 rounded-box border border-field-border"
				bind:this={wrapper}
				onscrollcapture={onScroll}
			>
				{#if !view.loaded}
					<div class="p-4" aria-busy="true"><Skeleton lines={6} /></div>
				{:else if !view.lines.length}
					<EmptyState
						title={filtered ? m.logs_none_match() : m.logs_empty()}
						text={filtered ? undefined : m.logs_empty_text()}
					>
						{#snippet action()}
							{#if filtered}<Button onclick={clearFilters}>{m.logs_clear_filters()}</Button>{:else}
								<a
									href={loggingHref}
									class="btn btn-outline border-field-border {FOCUS_RING} {TARGET_MIN}"
									>{m.logs_empty_action()}</a
								>{/if}
						{/snippet}
					</EmptyState>
				{:else}
					<!-- Announcements are off: a live stream read aloud is unusable (§10.10.12). -->
					<div role="log" aria-live="off" aria-label={m.logs_list_label()} class="h-full">
						{#key wrapped}
							<VirtualList
								items={view.lines}
								getKey={keyOf}
								label={m.logs_list_label()}
								measureRows
								estimateSize={wrapped && wrap === null ? ROW_TWO_LINES : 28}
								bind:activeIndex
								onActivate={(_, l) => toggle(keyOf(l))}
								class="font-mono text-small"
							>
								{#snippet row(l)}
									{@const Icon = ICONS[l.level]}
									{@const open = expanded.has(keyOf(l))}
									<div class="px-2 py-1 {TONE[l.level]}">
										<div
											class="logs-line flex {open || wrap === true
												? 'logs-wrap'
												: wrap === false
													? 'logs-one'
													: ''}"
										>
											<span class="shrink-0 text-muted-text"
												>{formatTime(l.ts, utc, displayTimeZone())}</span
											>
											<span class="logs-level inline-flex shrink-0 items-center gap-1">
												<Icon aria-hidden="true" />{levelName(l.level)}
											</span>
											<span class="logs-component shrink-0 text-muted-text"
												>{safe(l.component, 32)}</span
											>
											<span class="logs-msg min-w-0">
												{safe(l.msg || l.event || l.target, open ? FIELD_MAX : undefined)}
												{#if l.repeated}<span class="text-muted-text">
														{m.logs_repeated({
															count: l.repeated.count,
															seconds: l.repeated.seconds
														})}</span
													>{/if}
												{#if l.truncated}<span class="text-muted-text"> {m.logs_cut()}</span>{/if}
											</span>
										</div>
										{#if open}
											<dl class="mt-1 logs-fields gap-x-3 ps-4 text-muted-text">
												{#each l.fields as f, n (n)}
													<dt>{safe(f.name, 64)}</dt>
													<dd class="break-words whitespace-pre-wrap">
														{safe(f.value, FIELD_MAX)}
													</dd>
												{:else}
													<dd class="col-span-2">{m.logs_no_fields()}</dd>
												{/each}
											</dl>
										{/if}
									</div>
								{/snippet}
							</VirtualList>
						{/key}
					</div>
				{/if}
				{#if !view.following && view.unseen > 0}
					<div class="absolute inset-x-0 bottom-3 flex justify-center">
						<Button variant="primary" dense onclick={jump}
							>{m.logs_new_lines({ count: view.unseen })}</Button
						>
					</div>
				{/if}
			</div>
			<!-- The live region says only "Paused"; the changing count sits outside it. -->
			<div class="flex flex-wrap gap-x-4 text-small text-muted-text">
				<span role="status"
					>{[view.paused ? m.logs_paused_short() : '', notice].filter(Boolean).join(' · ')}</span
				>
				{#if view.paused}<span>{m.logs_paused({ count: view.held })}</span>{/if}
			</div>
		</div>
	</section>
</div>

<style>
	.logs-fields {
		display: grid;
		grid-template-columns: max-content 1fr;
	}
	.logs {
		height: calc(100dvh - 8rem);
		min-height: calc(var(--row-height-comfortable) * 8);
	}
	@media (min-width: 64rem) {
		.logs-toolbar {
			flex-wrap: nowrap;
		}
		.logs-toolbar .logs-field:has(:global(.logs-filter)) {
			flex: 1 1 auto;
			max-width: var(--manuscript-width);
		}
		.logs-toolbar :global(.logs-filter) {
			width: 100%;
		}
	}
	.logs-level {
		width: 5rem;
	}
	/*
	 * A row is a phone-first two-line layout: time, level and component, then the whole message
	 * wrapped. From 640 px it is one line with the message truncated. The wrap toggle forces either
	 * (`logs-wrap`, `logs-one`); the default is decided here, in CSS, so nothing moves after paint.
	 */
	.logs-toolbar {
		align-items: flex-end;
	}
	.logs-field {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 0.5);
	}
	.logs-line {
		flex-wrap: wrap;
		column-gap: calc(var(--spacing) * 2);
		overflow: hidden;
	}
	.logs-component {
		max-width: 8rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.logs-msg {
		flex: 1 1 100%;
		overflow-wrap: anywhere;
		white-space: pre-wrap;
	}
	.logs-field {
		flex: 1 1 100%;
	}
	.logs-field :global(select),
	.logs-field :global(.logs-filter) {
		width: 100%;
		max-width: none;
	}
	.logs-wrap .logs-msg {
		flex-basis: 100%;
	}
	.logs-one {
		flex-wrap: nowrap;
	}
	.logs-one .logs-msg {
		flex-basis: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	@media (min-width: 40rem) {
		.logs-field {
			flex: none;
		}
		.logs-field :global(select) {
			width: auto;
		}
		.logs-field :global(.logs-filter) {
			width: 16rem;
		}
		.logs-line:not(.logs-wrap) {
			flex-wrap: nowrap;
		}
		.logs-line:not(.logs-wrap) .logs-msg {
			flex-basis: 0;
			overflow: hidden;
			text-overflow: ellipsis;
			white-space: nowrap;
		}
		.logs-component {
			width: 6rem;
		}
		.logs-wrap .logs-msg {
			flex-basis: 0;
		}
	}
</style>
