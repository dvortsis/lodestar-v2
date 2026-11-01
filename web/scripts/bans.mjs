// Shared logic for the lint bans (§10.18: never hard-code colours; tokens only; §10.2 spacing ramp).
// Used by scripts/check-bans.mjs and by the ESLint config.

/** Hex colour literal such as #fff, #9381B6 or #9381B6cc (not part of a word or an entity). */
export const HEX_COLOUR =
	/(?<![\w&])#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{4}|[0-9a-fA-F]{3})\b/;

/** Tailwind arbitrary value: `w-[13px]`, `bg-[#fff]`, `!mt-[2px]`, `[mask-type:alpha]`. */
export const ARBITRARY_VALUE =
	/(?:^|[\s"'`:!])-?[a-z][a-z0-9-]*-\[[^\]\s]+\]|(?:^|[\s"'`:!])\[[a-z-]+:[^\]\s]+\]/;

/**
 * A `text-*` size class outside the §8 scale (`caption`, `small`, `body`, `body-lg`, `title`,
 * `display`): Tailwind's default steps are reset (`--text-*: initial`), so `text-h2` or `text-lg`
 * compiles to nothing and the text silently renders unstyled (M5 review B4/P11). Colours
 * (`text-muted-text`), alignment and wrapping utilities are not sizes and stay allowed.
 */
export const OFF_SCALE_TEXT =
	/(?:^|[\s"'`:!(])text-(?:xs|sm|base|lg|xl|[2-9]xl|h[1-6]|heading|headline|subtitle|large|huge|tiny|md|medium)(?=$|[\s"'`)!;=/,}])/;

/**
 * Spacing steps allowed in utilities (design-tokens.md §5): the §10.2 ramp as Tailwind steps,
 * where step n is n x 4px. Anything else (`p-11`, `gap-15`, `m-3.5`, `w-64`) is off the ramp.
 * Fixed sizes are not steps: they are named layout tokens from design-tokens.md §7, written
 * with Tailwind's variable shorthand (`w-(--rail-width)`, `max-w-(--manuscript-width)`) or as
 * `var(--rail-width)` in a style binding. Keywords (`w-full`, `h-px`) and fractions (`w-1/2`)
 * are not steps either and stay allowed.
 */
export const ALLOWED_STEPS = new Set([
	'0',
	'0.5',
	'1',
	'1.5',
	'2',
	'2.5',
	'3',
	'4',
	'5',
	'6',
	'7',
	'8',
	'9',
	'10',
	'12',
	'13',
	'14'
]);

/** Utilities that take a numeric spacing step. Keywords (`w-full`, `h-px`, `gap-x-px`) and fractions (`w-1/2`) are not steps. */
const SIDES = '[xytrblse]?';
const SPACING_PREFIX = `(?:[pm]${SIDES}|gap(?:-[xy])?|space-[xy]|inset(?:-[xy])?|top|right|bottom|left|start|end|size|(?:min-|max-)?[wh]|basis|translate-[xy]|scroll-[pm]${SIDES})`;
const SPACING_UTILITY = new RegExp(
	`(?:^|[\\s"'\`:!(])-?${SPACING_PREFIX}-(\\d+(?:\\.\\d+)?)(?=$|[\\s"'\`)!])`,
	'g'
);

/** Numeric spacing steps in `text` that are not on the ramp, e.g. ['11'] for `p-11`. */
export function offRampSteps(text) {
	return [...text.matchAll(SPACING_UTILITY)].map((m) => m[1]).filter((n) => !ALLOWED_STEPS.has(n));
}

export const hasOffRampStep = (text) => offRampSteps(text).length > 0;

/** Attributes whose values are fragments or identifiers, never colours (`href="#add"`). */
export const FRAGMENT_ATTRIBUTES = /^(?:href|xlink:href|id|for|name|popovertarget|aria-[a-z]+)$/;

/** Paths exempt from the bans (posix, relative to web/). */
export const EXEMPT = [/^src\/lib\/theme\//, /^src\/lib\/api\/schema\.d\.ts$/, /\.generated\./];

export const isExempt = (file) => EXEMPT.some((re) => re.test(file));

/** Blank out comments, keeping line breaks so reported line numbers stay right. */
export function stripComments(source) {
	const blank = (m) => m.replace(/[^\n]/g, ' ');
	return source
		.replace(/<!--[\s\S]*?-->/g, blank)
		.replace(/\/\*[\s\S]*?\*\//g, blank)
		.replace(/(^|[^:\w"'`])\/\/[^\n]*/g, (m, lead) => lead + blank(m.slice(lead.length)));
}

/** Remove `href="#x"`-style attribute values, which look like hex colours but aren't. */
function stripFragmentValues(line) {
	return line.replace(/([\w:-]+)\s*=\s*(?:"[^"]*"|'[^']*')/g, (m, name) =>
		FRAGMENT_ATTRIBUTES.test(name) ? ' '.repeat(m.length) : m
	);
}

/** @returns {{ line: number, kind: 'hex' | 'arbitrary' | 'spacing' | 'text' }[]} */
export function findViolations(source) {
	const found = [];
	stripComments(source)
		.split('\n')
		.forEach((line, i) => {
			if (HEX_COLOUR.test(stripFragmentValues(line))) found.push({ line: i + 1, kind: 'hex' });
			if (ARBITRARY_VALUE.test(line)) found.push({ line: i + 1, kind: 'arbitrary' });
			if (hasOffRampStep(line)) found.push({ line: i + 1, kind: 'spacing' });
			if (OFF_SCALE_TEXT.test(line)) found.push({ line: i + 1, kind: 'text' });
		});
	return found;
}
