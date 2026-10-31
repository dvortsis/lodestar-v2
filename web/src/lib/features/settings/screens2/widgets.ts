// The controls the connection screens stand in for the JSON fallback, by field name. Each loads
// when its field is on the page (`LazyWidget.svelte`).
import type { Component } from 'svelte';
import type { WidgetProps } from '../form/widgets';

type Load = () => Promise<{ default: Component<WidgetProps> }>;

export const WIDGETS: Record<string, Load> = {
	clients: () => import('./ClientsWidget.svelte'),
	default_client: () => import('./ClientSelectWidget.svelte'),
	route_alerts: () => import('./ChannelsWidget.svelte'),
	route_updates: () => import('./ChannelsWidget.svelte'),
	route_health: () => import('./ChannelsWidget.svelte'),
	digest_channels: () => import('./ChannelsWidget.svelte')
};
