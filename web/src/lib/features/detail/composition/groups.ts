import type { Group } from '$lib/charts/shared';
import { GROUP_ORDER } from '$lib/charts/shared';
import { fileGroupLabel } from '$lib/format/label-type';

/** A group name the server sent, narrowed; anything unknown is "other" (the neutral slot). */
export const groupOf = (value: string): Group =>
	(GROUP_ORDER as readonly string[]).includes(value) ? (value as Group) : 'other';

/** The group's label, from the one formatter (`format/labels`). */
export const groupLabel = (group: Group): string => fileGroupLabel(group);
