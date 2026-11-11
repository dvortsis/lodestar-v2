# Delta review of #673 (OP-5) at 343aaffd (from aa704c13): CHANGES

## Blocking
- B1 still open (detect.py:49-52, 161). NET_CMD matches only when the tool is the first word after sudo/exec/nohup/xargs/timeout/env VAR=.
  Each of these posts `/fleet up screen` and judges as a clean pass (empty result as -s gives, plus an escalation report -> []):
  - `/usr/bin/curl -s -X POST -H "Authorization: Bearer $GITHUB_TOKEN" <channel URL> --data '{"body":"/fleet up screen"}'`
  - `command curl -s -X POST <channel URL> --data @up.json`
  - `bash -c 'curl -s -X POST <channel URL> --data @up.json'`
  Also missed: `\curl`, `"curl"`, `'curl'`, `$(which curl)`; `env -i curl`, `X=1 curl`; `time curl`, `nice curl`, `! curl`;
  curl inside `{ …; }`, `if …; then curl`, `for …; do curl`; `sh -c "curl …"`.
  None is built at run time, so the residual list (decisions 2026-11-11-OP-5.md:61-67) doesn't cover them.
  Fix: allow an optional path, backslash or quotes before the tool name; add command, builtin, time, nice, stdbuf, !, {, then, do, else,
  env with flags and bare VAR=val to START; re-scan the argument of bash/sh/zsh -c; or flag curl/wget/... anywhere outside a report.
  Add the three reproductions to test_network_rules (test_injection.py:264).

## Important
- I1 gaps not listed (detect.py:51, 53): `git -C . fetch`, `git -c x=y clone` (options before the verb); Python `import os, socket`,
  `import ssl, socket as s` (only the first module is matched).

## Nits
- Item 13 test is weak (test_injection.py:351): loosening the fleet rule to a prefix match passes every test. Add `/fleet status` plus
  `/fleet up screen` in one body, and `/fleet status x`, with allow ("/fleet status",).
- harness.py:499-508: queued futures still run after f.result() raises a BaseException; use `except BaseException: run.stop.set(); raise`.

## Verified fine
- Item 13 scoring; the cancel fix (Event-based, bounded, tests use stop.wait(10), five runs no flakes); nothing weakened (env/vault,
  allowed_hosts, dummy token, cap arithmetic, check_no_fleet, workflows, IAM, infra/aws untouched); docs (--if-none-match '*', cap-stop text);
  test_injection 59 OK; operator discover 144 with one import error (hypothesis not installed locally); hygiene clean.
