Re: (new task)

From: dashboard session, 2026-11-07. Proposal only. The owner's task prompt is the go-ahead.

Goal: a first, tiny pilot of rented test machines. Prove the routine (create, run one harmless command, read the result, destroy) before any real test.
Provider proposed: Vultr (Linux and Windows machines, billed by the hour, has an API and the vultr-cli tool). If the owner chose a different provider, use that one and say so in the reply.

## Hard rules for this task
1. Run on the Windows PC only. Do NOT touch the Unraid box.
2. The owner creates the account, the payment method and the API key by hand. Never ask for, print, save, paste or write the key anywhere in the mailbox. It lives only in an environment variable on the Windows PC (for example VULTR_API_KEY), set by the owner.
3. The owner sets a monthly spending limit or alert on the account first. If the owner has not, stop and say so.
4. No Lodestar crawler and no DHT or BitTorrent traffic on any rented machine in this pilot. Plain Linux, Docker hello-world only. (Cloud providers may flag DHT-style traffic as network scanning. That question is for the owner and the provider's support, not this task.)
5. Name every resource you create with the prefix "lodestar-pilot-" so they are easy to find and delete.
6. Ask the owner before creating anything that bills, and before destroying anything. Show the exact plan (region, size, image, hourly price) first.
7. Do not write VM IP addresses, passwords or key text into any report. Write "[withheld]".

## Steps
1. Check that vultr-cli is installed and that the API key variable is set (report only "set" or "not set"). If the CLI is missing, tell the owner how you propose to install it and wait.
2. List available regions and the smallest Linux plan with at least 2 vCPU and 4 GB memory. Report plan name, hourly price and monthly price. Wait for the owner's go.
3. Create ONE Linux machine named lodestar-pilot-linux-1 with a Docker-ready image, and SSH-key login only (no password login). Use a new SSH key made for this pilot; keep the private key on the Windows PC only.
4. Over SSH, run: docker --version and docker run --rm hello-world. Capture the output.
5. Report the time from "create" to "hello-world worked".
6. Destroy the machine and its disk. Then list all instances, block storage, snapshots, reserved IPs and SSH keys with the prefix lodestar-pilot- and confirm none remain. (Remove the pilot SSH key from the account too.)
7. Write the report to from-antigravity/002-cloud-vm-pilot.md: provider, plan, hourly price, minutes billed (from create to destroy), whether hello-world worked, the leftover-resource check result, and every command that failed. Tell the owner the file name.

Expected cost of the whole pilot: under one dollar.
