# V6-cluster1: Cluster CA, mTLS and one-time join tokens (remote workers, part 1 of 6)
**Model:** Opus (security-critical: PKI, authentication). **Branch:** `slice/V6-cluster1`.

**Spec:**
- `docs/architecture.md` §19 in full, especially §19.2 (topology and protocol) and §19.6 (joining, security and lifecycle); §12 (security); §16 (cluster and leases entries);
- `docs/build-plan.md` P4 (remote workers v1, PR 1 of 6);
- `crates/cluster/src/{lib.rs,leases.rs}` (the existing leases work).

## Own
- `crates/cluster/**`. This is new code beside leases.rs; don't break its API.
- Its tests, and a decisions file.
- **Don't wire** it into `crates/lodestar` yet. PR 2 (protocol) does that. Expose the public API, and leave a TODO naming V6-cluster2.

## Build
1. **The cluster CA.**
   - The coordinator generates a CA keypair on first use: Ed25519 or P-256, whichever rustls supports best. Store it encrypted at rest with the existing seal key mechanism, if there's a public API for it; otherwise say what's needed.
   - Issue short-lived leaf certs for workers. Renew before expiry.
   - Revocation: a list of revoked serials, checked on every handshake.
   - Rotation: the CA can be rotated, with an overlap window.
2. **mTLS** with rustls: both sides verify. Pin to the cluster CA only, never the system roots. TLS 1.3 only.
3. **One-time join tokens:**
   - high entropy, short TTL, single use, bound to an intended worker name;
   - shown once;
   - stored only as a hash;
   - redeemed over a TLS channel where the worker verifies the coordinator by a CA fingerprint embedded in the token. That's what stops a man-in-the-middle on first join.

   The worker gets its cert and key on redemption. Replay is refused.
4. **Persistence** goes through `crates/store`. If a table is needed, **don't add a migration**: B8 owns the M5 migration. Use the `settings` key-value store or files under `<data>/cluster/`, and say which in the decisions file.

**Security:**
- constant-time token comparison;
- no secrets in logs (see §12.1's never-logged rules);
- zeroise key material where practical;
- every refusal path tested;
- fuzz or property tests for token parsing.

## Common rules
- **Bar and precedence:** `runner/standards.md` is the bar. Read CLAUDE.md, `docs/owner-decisions.md`, and the architecture sections cited.
- **Decisions** go in a new `docs/decisions/<YYYY-MM-DD>-<slice-id>.md` (see `docs/decisions/README.md`), not in §16.
- **Before pushing,** merge `origin/main`; never rebase. Run every check CLAUDE.md lists for the files you touch.
- **Scope:** stay inside **Own**. Shared-file edits needed elsewhere go in the PR description.
- **PRs** stay at or under about 1,500 changed lines, or state the reason in the description.

## Done when
- Workspace checks pass, including adversarial tests: an expired or reused token, a wrong CA, a revoked cert, a clock skew window.
- The PR is titled "V6-cluster1: Cluster CA, mTLS and one-time join tokens", with the threat model in the description.
