<script lang="ts">
	// Clear usage data (§10.10.7): destructive, so it runs on its own, never as part of Save. A
	// dialog shows the exact count first and focus starts on Cancel; nothing is deleted without it.
	import { ApiError, api } from '$lib/api/client';
	import { formatCount } from '$lib/format';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { ask } from '../form/dialogs';

	let busy = $state(false);
	let text = $state('');
	let failed = $state(false);

	const say = (message: string, bad = false) => {
		text = message;
		failed = bad;
	};

	async function clear() {
		busy = true;
		say('');
		try {
			const { queries } = await api.get('/usage');
			if (queries === 0) return say(m.settings_screen_usage_empty());
			const count = formatCount(queries);
			const answer = await ask({
				title: m.settings_screen_usage_title(),
				body: [m.settings_screen_usage_body({ count }), m.settings_screen_usage_undone()],
				actions: [
					{ id: 'cancel', label: m.settings_form_cancel() },
					{
						id: 'clear',
						label: m.settings_screen_usage_confirm({ shown: count }),
						variant: 'danger'
					}
				],
				initial: 'cancel'
			});
			if (answer !== 'clear') return;
			const done = await api.post('/usage/clear', { body: { confirm: true } });
			say(m.settings_screen_usage_done({ count: formatCount(done.queries) }));
		} catch (e) {
			say(
				e instanceof ApiError && e.message ? e.message : m.settings_screen_request_failed(),
				true
			);
		} finally {
			busy = false;
		}
	}
</script>

<div class="flex flex-col gap-2" data-testid="danger-zone">
	<h3 class="text-label font-semibold">{m.settings_screen_usage_zone()}</h3>
	<p class="text-muted-text">{m.settings_screen_usage_help()}</p>
	<div class="flex flex-wrap items-center gap-3">
		<Button variant="danger" loading={busy} onclick={clear}
			>{m.settings_screen_usage_clear()}</Button
		>
		<p role="status" class="text-small {failed ? 'text-error' : 'text-success'}">{text}</p>
	</div>
</div>
