# X-fleet-polish decisions

Two follow-ups from the runner's reviews of #661 (OP-3b): rv-661-0cc76033 N5 and the f75a2109 delta. Neither loosens an owner or security decision, and neither changes an owner-set number.

- **The Docker proxy's policy model reads exactly one pattern per ACL line** (`infra/fleet/tests/test_block.py` `policy()`).
  - HAProxy ORs every pattern on an ACL line. The model read only `words[5]`, so a second pattern added to a `path` or `query` line would have gone unmodelled, and the allowed and denied tables would have kept passing.
  - A `path` line must now be exactly `acl <name> path -m reg <pattern>`, and a `query` line `acl <name> query -m str <value>`. Any other shape fails the test.
  - A new test adds a second pattern, or a trailing flag, to each kind of line and expects the model to refuse it.
  - Mutation-checked: dropping either length check lets the new test pass, so the test fails.
- **The OP-3b decisions file now says when a failing step removes the proxy** (a shared edit the runner approved). `start-block.sh` and `arm-switch.sh` remove it on a failure from the step that starts Lodestar onward, which is where they set `started`.
  - A start-block failure before that point hasn't started a proxy yet.
  - An arm-switch failure before it leaves the last segment's proxy running. The block's end path removes it later: `copy-off`, or `arm-switch` after segment 4.

  The old wording, "removes it at once too", didn't say this. The code is unchanged.
