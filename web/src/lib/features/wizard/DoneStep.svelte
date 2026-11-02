<script lang="ts">
	// Step 9: where to go next, with a link to Settings for everything that was skipped.
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import { STEPS, type Progress } from './steps';

	let { progress }: { progress: Progress } = $props();

	const names = {
		network: () => m.wizard_step_network(),
		how: () => m.wizard_step_how(),
		intensity: () => m.wizard_step_intensity(),
		storage: () => m.wizard_step_storage(),
		download: () => m.wizard_step_download(),
		packs: () => m.wizard_step_packs(),
		blocklists: () => m.blocklists_wizard_title(),
		updates: () => m.wizard_step_updates(),
		done: () => m.wizard_step_done()
	};
	const skipped = $derived(STEPS.filter((s) => progress.skipped.includes(s.id)));
</script>

<p class="text-body">{m.wizard_done_lead()}</p>
{#if progress.restart}
	<p class="text-body" data-testid="wizard-restart">{m.wizard_done_restart()}</p>
{/if}

{#if skipped.length}
	<section class="flex flex-col gap-2" data-testid="wizard-skipped">
		<h3 class="text-body-lg font-semibold">{m.wizard_done_skipped()}</h3>
		<ul class="flex flex-col gap-1 ps-4 text-body">
			{#each skipped as step (step.id)}
				<li class="list-disc">
					<a class="link ls-focus" href={resolve('/settings/[section]', { section: step.settings })}
						>{names[step.id]()}</a
					>
				</li>
			{/each}
		</ul>
	</section>
{/if}

<div class="flex flex-wrap gap-3">
	<a class="btn btn-primary" href={resolve('/search')}>{m.auth_welcome_search()}</a>
	<a class="btn btn-ghost" href={resolve('/settings')}>{m.auth_welcome_settings()}</a>
</div>
