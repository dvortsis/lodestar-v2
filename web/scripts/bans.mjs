// Shared patterns for the lint bans (§10.18: never hard-code colours; tokens only).
// Used by scripts/check-bans.mjs and by the ESLint config.

/** Hex colour literal such as #fff, #9381B6 or #9381B6cc. */
export const HEX_COLOUR = /#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{4}|[0-9a-fA-F]{3})\b/;

/** Tailwind arbitrary value: `w-[13px]`, `bg-[#fff]`, `[mask-type:alpha]`, `-top-[3px]`. */
export const ARBITRARY_VALUE =
	/(?:^|[\s"'`:])-?[a-z][a-z0-9-]*-\[[^\]\s]+\]|(?:^|[\s"'`])\[[a-z-]+:[^\]\s]+\]/;

/** Paths exempt from the bans (posix, relative to web/). */
export const EXEMPT = [/^src\/lib\/theme\//, /^src\/lib\/api\/schema\.d\.ts$/, /\.generated\./];

export const isExempt = (file) => EXEMPT.some((re) => re.test(file));
