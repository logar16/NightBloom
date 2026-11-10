import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './e2e/tests',
  testMatch: 'rust-desktop.spec.ts',
  workers: 1,
  retries: 0,
  reporter: 'list',
  outputDir: 'e2e/.results/rust-desktop',
  use: {
    ...devices['Desktop Chrome'],
    viewport: { width: 1440, height: 860 },
    headless: true,
    acceptDownloads: true,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    launchOptions: {
      args: ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
    },
  },
  projects: [{ name: 'chromium' }],
});
