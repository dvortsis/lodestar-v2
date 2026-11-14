<#
.SYNOPSIS
  The owner's one command for a new Linux test VM (X-test-vm): connect, set up, start Claude.

.DESCRIPTION
  Run on the owner's Windows PC after the coordinator has created a test VM with
  infra/dev-vm/devvm.py and given you its address and host key. Over a single SSH
  connection (one passphrase prompt) it:
    1. pins the VM's host key in its own known-hosts file
       ($HOME\.ssh\lodestar_testvm_known_hosts), so there's no fingerprint to compare
       by eye and an old key for a reused address can't get in the way;
    2. waits for the VM's first-boot setup to finish (at most 30 minutes);
    3. fetches the test tools into ~/testkit;
    4. asks for your PIA login, only if this VM doesn't have one yet. You type it;
       it never reaches the coordinator, the chat or git (pia-login.sh --keep:
       stored root-only on the VM, owner 2026-11-12T16:55Z);
    5. starts Claude in tmux with Remote Control and its first message, then
       opens it so you can sign in.

  What stays yours: typing the PIA login, and signing Claude in through the link
  it shows. When Claude is running, press Ctrl+B, let go, then D to leave it running.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\Connect-TestVm.ps1 -Ip 192.0.2.10 -HostKey "ssh-ed25519 AAAA..."
#>
param(
    [Parameter(Mandatory = $true)][string]$Ip,
    # The VM's public host key line from devvm.py's work folder (host_ed25519.pub).
    [Parameter(Mandatory = $true)][string]$HostKey,
    [string]$KeyFile = (Join-Path $HOME '.ssh\lodestar_akamai_ed25519'),
    [string]$User = 'lodestar',
    [string]$PiaRegion = 'us_new_york_city',
    [string]$SessionName = 'claude-dev test VM',
    [string]$FirstMessage = "You're a test session on this VM for the X-test-vm coordinator. Wait for the coordinator's brief before doing anything.",
    [string]$Branch = 'claude/dreamy-edison-xlmtoy'
)
$ErrorActionPreference = 'Stop'

# Check the inputs before anything touches the network.
$addr = $null
if (-not [System.Net.IPAddress]::TryParse($Ip, [ref]$addr) -or $addr.AddressFamily -ne 'InterNetwork') {
    throw "-Ip must be an IPv4 address, got '$Ip'"
}
$parts = $HostKey.Trim() -split '\s+'
if ($parts.Count -lt 2 -or $parts[0] -ne 'ssh-ed25519' -or $parts[1] -notmatch '^[A-Za-z0-9+/]+={0,2}$') {
    throw '-HostKey must be the "ssh-ed25519 AAAA..." line the coordinator gave you'
}
if ($PiaRegion -notmatch '^[a-z0-9_]+$') { throw "-PiaRegion looks wrong: '$PiaRegion'" }
if ($Branch -notmatch '^[A-Za-z0-9._/-]+$') { throw "-Branch looks wrong: '$Branch'" }
if (-not (Test-Path -LiteralPath $KeyFile)) { throw "SSH key not found: $KeyFile (pass -KeyFile)" }
if (-not (Get-Command ssh -ErrorAction SilentlyContinue)) {
    throw 'ssh not found. Install it: Settings > System > Optional features > OpenSSH Client.'
}

# 1. Pin the host key: drop any old line for this address, then add the new one.
$sshDir = Join-Path $HOME '.ssh'
if (-not (Test-Path -LiteralPath $sshDir)) { New-Item -ItemType Directory -Path $sshDir | Out-Null }
$kh = Join-Path $sshDir 'lodestar_testvm_known_hosts'
$lines = @()
if (Test-Path -LiteralPath $kh) {
    $lines = @(Get-Content -LiteralPath $kh | Where-Object { $_ -and -not $_.StartsWith("$Ip ") })
}
$lines += "$Ip $($parts[0]) $($parts[1])"
[System.IO.File]::WriteAllText($kh, (($lines -join "`n") + "`n"))

# 2-5 run on the VM. Values are passed base64-encoded, so no quoting can break them.
function ConvertTo-B64([string]$s) { [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($s)) }
$remote = @'
#!/usr/bin/env bash
set -euo pipefail
d() { printf %s "$1" | base64 -d; }
BRANCH=$(d '__BRANCH__'); REGION=$(d '__REGION__'); NAME=$(d '__NAME__'); FIRST=$(d '__FIRST__')

echo "Connected. Waiting for the machine's setup to finish (usually about 10 minutes after it was created)..."
status=/var/lib/devvm-bootstrap.status
deadline=$((SECONDS + 1800))
while :; do
  s=$(cat "$status" 2>/dev/null || true)
  case "$s" in
    ok*) echo "Setup finished."; break ;;
    FAILED*) echo "Setup failed ($s). Tell Claude what this says." >&2; exit 1 ;;
  esac
  if (( SECONDS >= deadline )); then echo "Setup didn't finish within 30 minutes. Tell Claude." >&2; exit 1; fi
  sleep 15  # polling interval for a human-facing wait, not a test
done

if [ ! -d "$HOME/testkit" ]; then
  echo "Fetching the test tools..."
  git -C "$HOME/lodestar-v2" fetch -q origin "$BRANCH"
  git -C "$HOME/lodestar-v2" worktree add -q "$HOME/testkit" FETCH_HEAD
fi

if sudo test -s /home/lodestar/lodestar/secrets/wg0.conf; then
  echo "PIA is already set up on this machine."
else
  echo
  echo "Now enter your PIA login. Nothing appears while you type; that's normal."
  until sudo "$HOME/testkit/infra/dev-vm/pia-login.sh" --keep "$REGION"; do
    read -r -p "PIA setup didn't work. Try again? [Y/n] " a
    case "$a" in [nN]*) echo "Stopped. Tell Claude." >&2; exit 1 ;; esac
  done
fi

if ! tmux has-session -t claude 2>/dev/null; then
  {
    echo 'cd "$HOME/lodestar-v2"'
    echo "claude --remote-control $(printf %q "$NAME") --permission-mode auto $(printf %q "$FIRST")"
    echo 'exec bash'
  } > "$HOME/.devvm-claude.sh"
  tmux new-session -d -s claude "bash $HOME/.devvm-claude.sh"
fi

cat <<'EOT'

Claude is starting. In the next screen:
  1. If it asks, pick a theme and sign in: open the link it shows on your PC,
     sign in, copy the code and paste it back here (right-click pastes).
  2. If it asks whether to trust the folder, choose yes.
  3. Its first message is already filled in. If Claude sits at an empty prompt
     after sign-in, tell the coordinator in your Claude app.
  4. To leave it running: press Ctrl+B, let go, then press D. Then close this window.
EOT
read -r -p "Press Enter to open Claude... " _
exec tmux attach -t claude
'@
$remote = $remote.Replace('__BRANCH__', (ConvertTo-B64 $Branch))
$remote = $remote.Replace('__REGION__', (ConvertTo-B64 $PiaRegion))
$remote = $remote.Replace('__NAME__', (ConvertTo-B64 $SessionName))
$remote = $remote.Replace('__FIRST__', (ConvertTo-B64 $FirstMessage))
$remote = $remote.Replace("`r`n", "`n")
$cmd = "printf %s '$(ConvertTo-B64 $remote)' | base64 -d > ~/.devvm-connect.sh && bash ~/.devvm-connect.sh"

Write-Host "Connecting to $User@$Ip. Enter your SSH key's passphrase if asked."
& ssh -t -i $KeyFile `
    -o "UserKnownHostsFile=$kh" -o StrictHostKeyChecking=yes -o IdentitiesOnly=yes `
    -o ConnectTimeout=10 -o ConnectionAttempts=60 -o ServerAliveInterval=30 `
    "$User@$Ip" $cmd
if ($LASTEXITCODE -ne 0) {
    Write-Host "The connection ended with exit code $LASTEXITCODE. If that wasn't you detaching, tell Claude what the window showed."
}
