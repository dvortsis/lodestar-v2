// Snapshot policy (README "Snapshot policy"): which route is snapshotted, under which name, in
// which project. Everything else gets axe and a render check only.
import type { Density } from './matrix';
import type { RouteCase } from './routes';

export type Shots = 'matrix' | 'themes' | 'none';

/** The baseline name for this route in this project, or null when it has none. */
export function snapshotName(
	route: Pick<RouteCase, 'name' | 'key' | 'densityAware'>,
	opts: { shots: Shots; density: Density; themeId: string }
): string | null {
	switch (opts.shots) {
		case 'none':
			return null;
		case 'themes':
			// Riverstone's baselines belong to the bp1366 matrix projects.
			return route.key && !opts.themeId.startsWith('riverstone-')
				? `${route.name}--${opts.themeId}`
				: null;
		case 'matrix':
			return (opts.density === 'compact' ? route.densityAware : route.key) ? route.name : null;
	}
}

/** Full-page shots below 1920 px; the viewport only above, where pages are mostly empty margin. */
export const fullPageAt = (width: number) => width < 1920;
