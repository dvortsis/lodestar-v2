# X-operator-check8-compare decisions (#912)

Check 8 (`infra/operator/injection/harness.py`, `check_deployed`) refuses to start unless the deployed agent is what `main`'s files render to. It compared the agent's `tools` with `agent.json` value for value, which the Managed Agents API never returns. Found in review of `2026-11-14-X-operator-bash-allow.md` (its "Found in review" bullet). The runner made this a slice before the owner's first check 8 run (2026-11-15).

## What the API returns

- **Each `configs` entry's `type` is always returned,** equal to its `name`. It's optional in requests, the server infers it, and `agent.json` leaves it out on all three entries.
  - So the exact comparison refused whatever was deployed, even straight after `operator-apply.yml`.
  - Source: the Managed Agents tools reference, the `configs` row ("`type` is optional in requests ... and always present in responses").
- **The toolset's documented defaults:** a bare `agent_toolset_20260401` enables every tool with the `always_allow` policy, and a `configs` entry inherits what it omits from `default_config`. A response may spell these out where `agent.json` doesn't.
- **Not confirmed live:** this slice makes no API calls. The comparison accepts the documented forms; anything else refuses, so a shape the documentation doesn't describe fails closed and says to run `operator-apply.yml`.

## What check 8 compares now (`tools_match`)

- **The same tools, in the same order.** An added or removed tool refuses.
- **For the agent toolset,** each side is resolved before comparing:
  - `default_config` over the documented defaults (`TOOLSET_DEFAULTS`: `enabled: true`, `always_allow`);
  - each `configs` entry over that, with `type` equal to `name`, keyed by tool name so their order doesn't matter;
  - a tool only one side lists gets the toolset's defaults on the other.
- **Then the two must be equal as JSON** (sorted keys), so `0` or `1.0` never passes for a boolean. A changed `enabled`, permission policy or default refuses. So do:
  - a `type` that isn't the entry's `name`;
  - a duplicate entry, an entry with no name, or any field the toolset doesn't declare (a web setting such as `allowed_domains` included);
  - a `default_config` that isn't an object, `configs` that aren't a list, or an entry that isn't an object (refused, never raised);
  - a toolset of another type.
- **Any other tool kind** must equal its declaration exactly.
- **Unchanged:** the `version` and `system` checks, and the refusal message.
- **Not loosened to names:** a deployed toolset that behaves differently from `main`'s (a tool on or off, another policy) always refuses. Only values with no effect on behaviour are excused, because they're the documented defaults or the inferred `type`.

## Tests (`infra/operator/tests/test_injection.py`)

- **`test_the_agent_as_the_api_returns_it_passes`:** main's toolset in resolved form passes, both on its own and through a whole run. That form has each `type` filled in, the configs reversed, an inherited `enabled` spelled out, and an extra tool listed at the defaults. It fails with the previous exact comparison.
- **`test_every_other_difference_in_tools_refuses`:** 22 changes to the toolset and 2 to a custom tool, each refusing with the same message.
  - Among them: an added or removed tool, a changed permission, a tool turned on, a removed config, a changed default, a missing `default_config` or default policy, an extra config off the defaults, a mismatched `type`, a duplicate, unknown fields, values of the wrong shape, a custom tool in the toolset's place, and `0` or `1.0` for a boolean.
  - A changed system prompt, another version, and `tools` that aren't a list also refuse.
  - With a comparison that accepts any list, all 24 tool changes fail. The review's mutations each fail too: a missing `default_config` read as the declared one, `auto` as the default policy, no type guards, no exact match for other tool kinds, and plain `==` in place of the JSON comparison.

## Also in this PR

- `docs/decisions/2026-11-14-X-operator-check-notes.md`: its heading named "(#PR)"; it's #885.
