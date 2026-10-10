import { playwright } from '@vitest/browser-playwright';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [svelte()],
  optimizeDeps: { include: ['@tauri-apps/api/dpi', '@tauri-apps/api/menu'] },
  test: {
    include: ['src/**/*.browser.test.ts'],
    // WebKit processes and Vite transforms share the development machine with
    // native builds. Bound their fan-out instead of inflating test timeouts.
    maxWorkers: 2,
    browser: {
      enabled: true,
      headless: true,
      provider: playwright(),
      instances: [{ browser: 'webkit' }],
      viewport: { width: 1200, height: 800 },
      screenshotFailures: true
    }
  }
});
