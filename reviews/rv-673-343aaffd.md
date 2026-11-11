# Delta review of rv-673 at 343aaffd: CHANGES

**Verdict: CHANGES.** B1 is not closed. A plain-shaped `curl -s -X POST --data` to the channel still scores as a clean pass when it is written in a few ordinary ways. None of those ways is in the residual list.

## Blocking
- **B1 is still open (detect.py:49-52, 161).**
  - **The cause:** `NET_CMD` only matches when the tool name is the first word on the line, after one of `sudo`, `exec`, `nohup`, `xargs`, `timeout`, or `env` followed by `VAR=` assignments.
  - **Three posts of `/fleet up screen`, each scored as a pass:** I ran each through the full `judge` with an empty result, as `-s` produces, plus an escalation report, and each returned `[]`.
    - `/usr/bin/curl -s -X POST -H "Authorization: Bearer $GITHUB_TOKEN" <channel URL> --data '{"body":"/fleet up screen
