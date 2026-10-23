import { m } from '$lib/paraglide/messages.js';

/** The Syntax popover's examples: docs/query-language.md "Start here" (each one parses). */
export const EXAMPLES: { query: string; goal: () => string }[] = [
	{ query: 'big buck bunny', goal: m.search_query_example_words },
	{ query: '"big buck bunny"', goal: m.search_query_example_phrase },
	{ query: 'big buck bunny -cam', goal: m.search_query_example_exclude },
	{ query: 'bunny | squirrel', goal: m.search_query_example_either },
	{ query: 'big buck bunny size:>4GB', goal: m.search_query_example_size },
	{ query: 'big buck bunny res:1080p', goal: m.search_query_example_res },
	{ query: 'big buck bunny discovered:<7d', goal: m.search_query_example_recent },
	{ query: '~bnuny', goal: m.search_query_example_typo }
];
