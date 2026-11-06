// A copy of the approved consent texts: `docs/decisions/2026-11-02-SRC-4.md` "Draft consent texts"
// with owner §2.49's one change to the Account text applied. Kept apart from en.json on purpose, so
// a reworded message fails `consent.test.ts` until the owner has approved the new wording. Each text
// is `title`, the paragraphs and bullets in page order (`**bold**` marks stay), then the button.
// `{name}`, `{hosts}`, `{egress}` and `{network}` are filled in from the API.
export const approved = {
	external: {
		lines: [
			'{name} connects to outside services.',
			"When it's on, Lodestar contacts these sites on your behalf: {hosts}. They can see your IP address (or your VPN's) and when you connect, as they would if you visited them. Lodestar sends them search terms and info hashes, never your files or your account details.",
			"Traffic goes through this source's network setting ({egress}). If the list of sites changes in an update, you'll be asked again."
		],
		buttons: ['Turn on {name}', 'Cancel']
	},
	account: {
		lines: [
			'{name} signs in with your account.',
			"Enter the credentials {name} needs. Lodestar encrypts them on this server and uses them only to talk to {hosts}. They're never shown again, written to logs, included in a settings export or sent anywhere else. You can remove them at any time, and purging the source deletes them.",
			"Results found only through your account stay on this server: Lodestar never shares them through Torznab, Stremio, exports or public peer finding, because private trackers usually forbid it. If a public source reports the same torrent, it's treated as public."
		],
		buttons: ['Save and turn on', 'Cancel']
	},
	restricted: {
		lines: [
			'{name} can expose you to legal risk.',
			"{name} reaches sources that may index copyrighted or otherwise restricted material, or it announces your IP address to other peers. Depending on where you live, using it can break the law or your internet provider's terms, and other people can see that your address took part.",
			"Lodestar collects metadata only: names, sizes and hashes. It doesn't download content.",
			"**Use a VPN.** Set this source's network setting to your VPN, so the kill switch stops it if the VPN drops.",
			"You're responsible for whether using it is lawful where you are.",
			"Enter your password to turn it on. You'll be asked again if this notice changes."
		],
		buttons: ['Turn on {name}', 'Cancel']
	},
	overlay: {
		lines: [
			'{name} joins another network.',
			"{name} connects Lodestar to {network}, a separate peer-to-peer network. Other peers on it can see your IP address (or your VPN's), and what you search for may be visible to them. That network has its own content and its own risks, and Lodestar can't check what others share there.",
			'**Use a VPN** for this source, so the kill switch stops it if the VPN drops.',
			'What it finds is kept apart from your BitTorrent results, and **Purge** removes all of it in one step.',
			"Lodestar collects metadata only. It doesn't download content.",
			"Enter your password to turn it on. You'll be asked again if this notice changes."
		],
		buttons: ['Turn on {name}', 'Cancel']
	}
} as const;
