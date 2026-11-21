# X-testvm-runbook-r2 decisions

Runbook fixes from the 2026-11-20Z AWS lanes. No decision changes; both tighten how sessions work.

- **Windows lanes building tests on `c6a.xlarge` set a 16 GB pagefile and build with `-j1`.** The windata lane's test build hung Windows and dropped SSM. Two `rustc` processes of about 5.6 GB each didn't fit 8 GiB with the default pagefile. Timings taken during a build are not comparable (stops took 18–40 s instead of 0.4 s).
- **Teardown checks volumes by `tag:Purpose=X-test-vm`, never `tag:Project=Lodestar`.** Instances were already counted that way (`docs/decisions/2026-11-12-X-test-vm.md`), but the teardown steps still filtered volumes by `Project`, which the owner's fleet volumes share.
