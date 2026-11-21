// The plain names and one-line descriptions of the built-in rules and packs (I5): the API sends only
// their codes (`CoreRuleState`, `PackRuleState`, `PackState`), so the words are this module's
// messages, keyed by code. It loads on demand, with the Filters screen's first request (`names.svelte.ts`),
// so neither the screen's first view nor the change dialogs carry the texts. A code with no entry
// here has no name: the screens then show the code alone. `rule-text.test.ts` proves every
// built-in id in `crates/filter` has an entry.
import { m } from '$lib/paraglide/messages.js';

type Text = readonly [name: () => string, text: () => string];

const texts: Record<string, Text> = {
	// The child-safety core (§20.5).
	'CS-1a': [m.filters_rule_cs_1a, m.filters_rule_cs_1a_text],
	'CS-1b': [m.filters_rule_cs_1b, m.filters_rule_cs_1b_text],
	'CS-8a': [m.filters_rule_cs_8a, m.filters_rule_cs_8a_text],
	'CS-0a': [m.filters_rule_cs_0a, m.filters_rule_cs_0a_text],
	'CS-2a': [m.filters_rule_cs_2a, m.filters_rule_cs_2a_text],
	'CS-3a': [m.filters_rule_cs_3a, m.filters_rule_cs_3a_text],
	'CS-3b': [m.filters_rule_cs_3b, m.filters_rule_cs_3b_text],
	'CS-4a': [m.filters_rule_cs_4a, m.filters_rule_cs_4a_text],
	'CS-5a': [m.filters_rule_cs_5a, m.filters_rule_cs_5a_text],
	'CS-5b': [m.filters_rule_cs_5b, m.filters_rule_cs_5b_text],
	// The rule packs (§20.6).
	'MW-1': [m.filters_rule_mw_1, m.filters_rule_mw_1_text],
	'MW-2': [m.filters_rule_mw_2, m.filters_rule_mw_2_text],
	'MW-3': [m.filters_rule_mw_3, m.filters_rule_mw_3_text],
	'MW-4': [m.filters_rule_mw_4, m.filters_rule_mw_4_text],
	'MW-5': [m.filters_rule_mw_5, m.filters_rule_mw_5_text],
	'MW-6': [m.filters_rule_mw_6, m.filters_rule_mw_6_text],
	'LU-1': [m.filters_rule_lu_1, m.filters_rule_lu_1_text],
	'LU-2': [m.filters_rule_lu_2, m.filters_rule_lu_2_text],
	'LU-3': [m.filters_rule_lu_3, m.filters_rule_lu_3_text],
	'LU-4': [m.filters_rule_lu_4, m.filters_rule_lu_4_text],
	'FK-1': [m.filters_rule_fk_1, m.filters_rule_fk_1_text],
	'FK-2': [m.filters_rule_fk_2, m.filters_rule_fk_2_text],
	'FK-4': [m.filters_rule_fk_4, m.filters_rule_fk_4_text],
	'FK-5': [m.filters_rule_fk_5, m.filters_rule_fk_5_text],
	'SP-1': [m.filters_rule_sp_1, m.filters_rule_sp_1_text],
	'SP-2': [m.filters_rule_sp_2, m.filters_rule_sp_2_text],
	'SP-3': [m.filters_rule_sp_3, m.filters_rule_sp_3_text],
	'SP-4': [m.filters_rule_sp_4, m.filters_rule_sp_4_text],
	'SC-1': [m.filters_rule_sc_1, m.filters_rule_sc_1_text],
	'SC-2': [m.filters_rule_sc_2, m.filters_rule_sc_2_text],
	'UC-1': [m.filters_rule_uc_1, m.filters_rule_uc_1_text],
	'UC-2': [m.filters_rule_uc_2, m.filters_rule_uc_2_text],
	'UC-3': [m.filters_rule_uc_3, m.filters_rule_uc_3_text],
	// The packs: the names the wizard uses, and the Filters screen's descriptions.
	malware: [m.filters_pack_name_malware, m.filters_pack_info_malware],
	lures: [m.filters_pack_name_lures, m.filters_pack_info_lures],
	fakes: [m.filters_pack_name_fakes, m.filters_pack_info_fakes],
	spam: [m.filters_pack_name_spam, m.filters_pack_info_spam],
	scams: [m.filters_pack_name_scams, m.filters_pack_info_scams],
	'unauthorised-copy': [
		m.filters_pack_name_unauthorised_copy,
		m.filters_pack_info_unauthorised_copy
	]
};

/** Every code this module names, for the test that matches them against the built-in ids. */
export const ids = Object.keys(texts);
// Own keys only: a code the server sends is never read as `constructor` or another inherited name.
const entry = (id: string): Text | undefined => (Object.hasOwn(texts, id) ? texts[id] : undefined);
/** The plain name of a rule or pack, or null for a code this build has no words for. */
export const nameOf = (id: string): string | null => entry(id)?.[0]() ?? null;
/** What the rule or pack checks, in one line, or null. */
export const textOf = (id: string): string | null => entry(id)?.[1]() ?? null;
