// Shared logic for the lint bans (§10.18: never hard-code colours; tokens only).
// Used by scripts/check-bans.mjs and by the ESLint config.

/** Hex colour literal such as #fff, #9381B6 or #9381B6cc (not part of a word or an entity). */
export const HEX_COLOUR =
	/(?<![\w&])#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{4}|[0-9a-fA-F]{3})\b/;

/** Tailwind arbitrary value: `w-[13px]`, `bg-[#fff]`, `!mt-[2px]`, `[mask-type:alpha]`. */
export const ARBITRARY_VALUE =
	/(?:^|[\s"'`:!])-?[a-z][a-z0-9-]*-\[[^\]\s]+\]|(?:^|[\s"'`:!])\[[a-z-]+:[^\]\s]+\]/;

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

/** @returns {{ line: number, kind: 'hex' | 'arbitrary' }[]} */
export function findViolations(source) {
	const found = [];
	stripComments(source)
		.split('\n')
		.forEach((line, i) => {
			if (HEX_COLOUR.test(stripFragmentValues(line))) found.push({ line: i + 1, kind: 'hex' });
			if (ARBITRARY_VALUE.test(line)) found.push({ line: i + 1, kind: 'arbitrary' });
		});
	return found;
}
