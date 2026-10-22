// The axes of the Playwright project matrix (architecture §10.17, §10.15, design-tokens §7).
export const BREAKPOINTS = [
	{ name: '320', width: 320, height: 640 },
	{ name: '390', width: 390, height: 844 },
	{ name: '768', width: 768, height: 1024 },
	{ name: '1024', width: 1024, height: 768 },
	{ name: '1366', width: 1366, height: 768 },
	{ name: '1920', width: 1920, height: 1080 },
	{ name: '2560', width: 2560, height: 1440 }
] as const;

export const DENSITIES = ['comfortable', 'compact'] as const;
export type Density = (typeof DENSITIES)[number];

/** Breakpoint used by the theme-sample, reduced-motion and keyboard-only projects. */
export const DESKTOP = BREAKPOINTS.find((b) => b.name === '1366')!;
