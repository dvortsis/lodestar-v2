// The screens' icons, in one lazily loaded module: the navigation imports it after first paint, so
// the icons never count against a Settings route's first view (the 45 KB route limit). A screen
// registered by another slice can still set `icon` in its `screen.ts`.
import type { Component } from 'svelte';
import IconCrawler from '~icons/ph/gauge';
import IconGear from '~icons/ph/gear';
import IconGeneral from '~icons/ph/sliders';
import IconNetwork from '~icons/ph/network';
import IconSearch from '~icons/ph/magnifying-glass';
import IconStorage from '~icons/ph/hard-drives';

export const FALLBACK: Component = IconGear;

export const ICONS: Record<string, Component> = {
	crawler: IconCrawler,
	network: IconNetwork,
	storage: IconStorage,
	search: IconSearch,
	general: IconGeneral
};
