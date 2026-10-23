<script lang="ts">
	// The full-page frame of the sign-in and setup screens: the horizontal lockup (§10.19), the
	// page heading and one form. One column, usable from 320 px.
	import { onMount, type Snippet } from 'svelte';
	import { logos } from '$lib/brand/scenes.generated';
	import { m } from '$lib/paraglide/messages.js';

	interface Props {
		title: string;
		lead: string;
		children: Snippet;
	}

	let { title, lead, children }: Props = $props();

	// The lockup follows the active theme's mode, not the OS: the pre-paint script and the theme
	// switcher set `color-scheme` on <html>, so watch that.
	const root = typeof document === 'undefined' ? null : document.documentElement;
	const modeOf = () => (root?.style.colorScheme === 'dark' ? 'dark' : 'light');
	let mode = $state(modeOf());

	onMount(() => {
		if (!root) return;
		const observer = new MutationObserver(() => (mode = modeOf()));
		observer.observe(root, { attributes: true, attributeFilter: ['style', 'data-theme'] });
		return () => observer.disconnect();
	});

	const logo = $derived(logos.find((l) => l.id === 'horizontal' && l.mode === mode)!);
	const fallback = $derived(logo.src.webp.split(' ')[0]);
</script>

<div class="flex min-h-dvh flex-col items-center justify-center px-4 py-8">
	<div class="flex w-full max-w-sm flex-col gap-6">
		<picture>
			<source type="image/avif" srcset={logo.src.avif} />
			<img
				src={fallback}
				srcset={logo.src.webp}
				width={logo.width}
				height={logo.height}
				alt={m.auth_logo_alt()}
				decoding="async"
				class="h-auto max-w-full"
			/>
		</picture>
		<div class="flex flex-col gap-1">
			<h1 class="text-display font-semibold text-base-content">{title}</h1>
			<p class="text-body text-muted-text">{lead}</p>
		</div>
		{@render children()}
	</div>
</div>
