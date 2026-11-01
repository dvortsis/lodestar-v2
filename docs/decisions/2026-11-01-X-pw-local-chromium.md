# X-pw-local-chromium: local Chromium override

Decided by: the runner.

Cloud sessions have a Chromium build that differs from the one `@playwright/test` pins, so every web slice built a symlink folder. `web/playwright.config.ts` now reads `PW_CHROMIUM_PATH` and, when set, passes it as `launchOptions.executablePath` to every project. Unset (CI), the config is unchanged. Auto-detection was left out: an explicit variable cannot change CI by accident. Local screenshots are not authoritative; CI's baselines are.
