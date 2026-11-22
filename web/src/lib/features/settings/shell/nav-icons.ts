// The navigation icons of the hand-built screens that carry none of their own, one lazy module
// (X-settings-find, UI review P3): NavIcon loads it after the navigation shows, so no Settings
// first view carries these paths and a screen's registry entry stays as small as it was.
import type { Component } from 'svelte';
import IconAppearance from '~icons/ph/palette-bold';
import IconFilters from '~icons/ph/funnel-bold';
import IconBlocklists from '~icons/ph/prohibit-bold';
import IconJobs from '~icons/ph/timer-bold';
import IconKeyboard from '~icons/ph/keyboard-bold';
import IconSources from '~icons/ph/plugs-bold';

export const NAV_ICONS: Record<string, Component> = {
	appearance: IconAppearance,
	blocklists: IconBlocklists,
	filters: IconFilters,
	jobs: IconJobs,
	keyboard: IconKeyboard,
	sources: IconSources
};
