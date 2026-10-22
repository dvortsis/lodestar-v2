import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: 'e2e',
	webServer: {
		command: 'npm run build && npm run preview -- --port 4173',
		env: { VITE_DEV_ROUTES: '1' },
		port: 4173
	},
	use: { baseURL: 'http://localhost:4173' }
});
