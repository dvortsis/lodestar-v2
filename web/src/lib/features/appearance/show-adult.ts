// The variable that sets `content.show_adult` from outside the app (B17), named on Appearance's
// first view when the switch is locked. A copy of the flows' `SHOW_ADULT_ENV`, so that view carries
// none of `filters/flows/**` (X-flows-door-2); `show-adult.test.ts` keeps the two equal.
export const SHOW_ADULT_ENV = 'LODESTAR_SHOW_ADULT';
