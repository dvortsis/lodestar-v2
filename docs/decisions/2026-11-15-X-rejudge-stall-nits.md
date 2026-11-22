# X-rejudge-stall-nits decisions

The two optional findings from #901's review, in `core_rejudge`'s `add_stall`. Both were the runner's to schedule; how each is solved is this PR's call (`docs/owner-decisions.md` §2.19).

## The share past the old end can't overflow

- **What it worked out:** the rows a merged range adds past the old end, `rows × beyond ÷ span`, in `i128`. With `rows` near `u64::MAX` and a range spanning most of the ids, the product passes `i128::MAX`.
- **Chosen: cap it.** The product is taken with `checked_mul`. When it can't be held, the share is the new range's `rows`. Since `beyond` is at most `span`, that is the share's largest value, and the exact one when the range follows on.
- **Not chosen: refusing the merge.** It would keep a second, overlapping range for the same ids, and count its rows twice.
- The stored count was already saturating, so nothing past this changes.

## The right range when several match

- **What it did:** the first range of its phase that the new one starts inside. Ranges of one phase can overlap once widening past `MAX_STALLS` has run, and then the first may not reach furthest.
- **Now:** of the ranges of its phase it starts inside, it merges into the one reaching furthest, with the earliest on a tie. That adds the fewest rows, and none twice, and leaves the others as they were.

## Tests

- `the_share_past_the_end_never_overflows`: a range from `i64::MIN` to `i64::MAX` with `u64::MAX` rows merges without overflow, at all its rows. Just inside the edge, the share is still exact.
- `a_range_inside_several_merges_into_the_furthest`: three overlapping phase-1 ranges and one phase-2 range. The new range merges into the furthest only, with its rows counted once; a tie goes to the earliest.
