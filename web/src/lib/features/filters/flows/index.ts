// The filters change flows (M-filters-dialogs). Components are imported by path
// (`$lib/features/filters/flows/ChangeDialog.svelte`, …), and lazily: none of this belongs in a
// route's first load. See README.md for how each screen mounts them.
export { createChangeFlow, type ChangeFlow, type FlowOptions } from './flow.svelte';
export type { FlowState, PasswordError } from './machine';
export {
	addAllow,
	addException,
	addOverride,
	addRule,
	canShadow,
	confirmAbuse,
	coreSwitch,
	falsePositive,
	floorRemoval,
	packSwitch,
	release,
	removeAllow,
	removeCs1,
	removeException,
	removeOverride,
	removeRule,
	reportAbuse,
	revealName,
	undoChange,
	updateRule,
	SHADOW_OPS,
	type AddRuleInput,
	type OverrideInput,
	type UpdateRuleInput
} from './changes';
export {
	createAdultConfirm,
	setShowAdult,
	SHOW_ADULT_ENV,
	type AdultConfirm,
	type ShowAdultResult
} from './adult.svelte';
export {
	nameWords,
	phraseQuery,
	suggestBlocks,
	torrentQuery,
	type BlockSuggestion,
	type BlockTarget,
	type BlockTier,
	type BlockUnavailable
} from './block';
export { adultErrorText, errorText } from './errors';
export type * from './types';
