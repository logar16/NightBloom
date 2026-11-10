import { defineConfig, devices } from '@playwright/test';

// Workers inherit this ID; independent Playwright invocations never share fixture data.
process.env.ASTROIMP_E2E_RUN_ID ??= String(process.pid);

export default defineConfig({
  testDir: './e2e/tests',
  globalSetup: './e2e/setup.ts',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list'], ['json', { outputFile: `e2e/.results/${process.env.ASTROIMP_E2E_RUN_ID}/results.json` }]],
  outputDir: `e2e/.results/${process.env.ASTROIMP_E2E_RUN_ID}/artifacts`,
  use: {
    ...devices['Desktop Chrome'],
    viewport: { width: 1440, height: 860 },
    headless: true,
    acceptDownloads: true,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    launchOptions: {
      args: process.env.ASTROIMP_PERF_STAGE
        ? ['--use-gl=angle', '--use-angle=d3d11']
        : ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
    },
  },
  projects: [{ name: 'react' }],
});
