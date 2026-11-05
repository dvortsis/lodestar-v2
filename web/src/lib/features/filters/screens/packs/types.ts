import type { components } from '$lib/api/schema';

type S = components['schemas'];
export type PackState = S['PackState'];
export type PackRuleState = S['PackRuleState'];
export type PendingPackVersion = S['PendingPackVersion'];
export type PackCorpus = S['PackCorpus'];
export type PackAcceptance = S['PackAcceptance'];
