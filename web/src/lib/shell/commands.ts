// The commands the shell itself owns: the palette shortcut and "Go to …" for each destination.
// Features register their own from their own files with `keys.register`. Loaded when the browser
// is idle (see Shell.svelte), so it stays out of the first load.
import { goto } from '$app/navigation';
import { keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';
import { DESTINATIONS } from './nav';
import { shell } from './shell-state.svelte';

// eslint-disable-next-line svelte/no-navigation-without-resolve -- no base path: the app is served from the site root (svelte.config.js sets no paths.base)
const defaultGo = (href: string) => void goto(href);

export function registerShellCommands(go: (href: string) => void = defaultGo) {
	const offs = [
		keys.register({
			id: 'palette.open',
			title: () => m.shell_cmd_palette(),
			section: () => m.shell_cmd_section_general(),
			allowInInput: true,
			palette: false,
			run: () => shell.togglePalette()
		}),
		...DESTINATIONS.map((d) =>
			keys.register({
				id: `nav.${d.id}`,
				title: () => m.shell_cmd_go({ destination: d.label() }),
				section: () => m.shell_cmd_section_navigate(),
				keywords: [d.id],
				run: () => go(d.href)
			})
		)
	];
	return () => offs.forEach((off) => off());
}
