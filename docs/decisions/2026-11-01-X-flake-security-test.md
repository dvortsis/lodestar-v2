# X-flake-security-test: settings/security vitest flake

- **Cause:** `security.test.ts` opens the password dialog, a lazy chunk (`confirm.ts` imports `PasswordDialog.svelte` on first use). In a full parallel `vitest run` its first, cold transform can outlast the test's 10 s wait. "Cancel … sends nothing" then fails with no dialog found. The import finishes later and mounts the dialog during the next test, so "an empty password sends nothing" finds two dialogs. Reproduced on the first of three full runs; both tests passed alone.
- **Fix (test only):** a `beforeAll` loads `PasswordDialog` and `Lockout` before any test. Assertions, waits and timeouts are unchanged, so "sends nothing" still checks `writes()` after the dialog has closed or shown its error.
- **Proof:** 5 full `npx vitest run` passes and 10 solo runs of the file (see PR).
