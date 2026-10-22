import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';

// @axe-core/playwright resolves its own copy of playwright-core, so its Page type is nominally
// different from ours; the objects are the same at runtime.
type AxePage = ConstructorParameters<typeof AxeBuilder>[0]['page'];

/** WCAG 2.2 AA, cumulative: axe tags A and AA of 2.0, 2.1 and 2.2. */
export const WCAG_22_AA = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'];

export interface A11yOptions {
	/** Limit the scan to these selectors. */
	include?: string[];
	/** Skip these selectors (use sparingly, with a comment saying why). */
	exclude?: string[];
	/** Rule ids to turn off (use sparingly, with a comment saying why). */
	disableRules?: string[];
	tags?: string[];
}

/** Fails with each violation's rule, impact, targets and help URL. */
export async function expectNoA11yViolations(page: Page, opts: A11yOptions = {}) {
	let axe = new AxeBuilder({ page: page as unknown as AxePage }).withTags(opts.tags ?? WCAG_22_AA);
	for (const s of opts.include ?? []) axe = axe.include(s);
	for (const s of opts.exclude ?? []) axe = axe.exclude(s);
	if (opts.disableRules?.length) axe = axe.disableRules(opts.disableRules);
	const { violations } = await axe.analyze();
	if (!violations.length) return;
	const lines = violations.flatMap((v) => [
		`${v.id} (${v.impact ?? 'n/a'}): ${v.help}`,
		`  ${v.helpUrl}`,
		...v.nodes.map((n) => `  target: ${n.target.join(' ')}`)
	]);
	throw new Error(`${violations.length} axe violation(s) on ${page.url()}:\n${lines.join('\n')}`);
}
