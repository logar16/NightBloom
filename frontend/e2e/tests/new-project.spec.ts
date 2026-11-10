import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures';

const position = { name: 'NGC 3324', ra: 159.3408, dec: -58.615 };
const coverage = {
  counts: { HST: 12, JWST: 3 },
  instruments: [{ mission: 'HST', instrument: 'WFC3', count: 12 }, { mission: 'JWST', instrument: 'NIRCAM', count: 3 }],
  frame: { width_arcmin: 8, height_arcmin: 4.5, aspect: '16:9', covering: 4 },
};
async function screenshot(page: Page, name: string) {
  if (!process.env.ASTROIMP_GOTO_SHOTS) return;
  await mkdir(process.env.ASTROIMP_GOTO_SHOTS, { recursive: true });
  await page.screenshot({ path: path.join(process.env.ASTROIMP_GOTO_SHOTS, name),
    style: '#toast { display:none !important; transition:none !important }' });
}

test.describe('New project', () => {
  test('Search - frame controls capture a coordinate project without a snapshot', async ({ page, studio, request }) => {
    let captured: Record<string, unknown> | null = null;
    const original = await (await request.get(`${studio.origin}/api/session`)).json();
    const project = { ...original, name: position.name, meta: null, edits: null,
      selection: { obs_ids: [] }, view: { ...original.view, source: 'coordinates', rotation_deg: 30,
        center_icrs_deg: [position.ra, position.dec], field_arcmin: [8, 4.5] } };
    await page.route('**/api/coordinates/resolve?*', route => route.fulfill({ json: position }));
    await page.route('**/api/coordinates/coverage?*', route => route.fulfill({ json: coverage }));
    await page.route('**/api/capture/coordinates', async route => {
      captured = route.request().postDataJSON() as Record<string, unknown>;
      await route.fulfill({ json: { id: 'coordinates', kind: 'capture', state: 'done', session: studio.session.id } });
    });
    await page.route('**/api/status', route => route.fulfill({ json: {
      aladin: false, session: studio.session.id, job: null, services: { cds: { ok: false } }, edits: null,
    } }));
    await page.getByRole('link', { name: 'NightBloom Home', exact: true }).click();
    await page.getByRole('button', { name: 'New project', exact: true }).click();
    await expect(page).toHaveURL(/#\/new$/);
    await page.getByRole('textbox', { name: 'Target name or coordinates' }).fill('NGC 3324');
    await page.getByRole('button', { name: 'Search', exact: true }).click();
    await expect(page.getByRole('heading', { name: position.name })).toHaveText(position.name);
    await expect(page.getByLabel('Archive coverage')).toContainText('HST12');
    await expect(page.getByLabel('Archive coverage')).toContainText('JWST3');
    await expect(page.getByRole('spinbutton', { name: 'Frame width' })).toHaveValue('8');
    for (const theme of ['dark', 'light']) {
      await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
      await screenshot(page, `new-project-results-${theme}.png`);
    }
    await page.getByRole('combobox', { name: 'Frame aspect' }).selectOption('3:2');
    await expect(page.getByRole('spinbutton', { name: 'Frame width' })).toHaveValue('8');
    await page.getByRole('spinbutton', { name: 'Frame width' }).fill('9');
    await page.getByRole('spinbutton', { name: 'Frame rotation' }).fill('30');
    await page.route(/\/api\/session(\?.*)?$/, route => route.fulfill({ json: project }));
    await page.getByRole('button', { name: 'Capture', exact: true }).click();
    await expect.poll(() => captured).toMatchObject({ ...position, width_arcmin: 9, aspect: '3:2', rotation_deg: 30, surveys_only: false });
    await expect(page.getByTestId('coordinate-backdrop')).toHaveAttribute('aria-label', 'RA and Dec sky grid');
    await expect(page.getByTestId('footprints')).toHaveCount(1);
    await expect(page.getByRole('button', { name: 'Re-grab image from Aladin (keeps search results)' })).toHaveCount(0);
    await expect(page.getByRole('spinbutton', { name: 'Capture rotation' })).toHaveValue('30');
    for (const theme of ['dark', 'light']) {
      await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
      await screenshot(page, `coordinate-sky-${theme}.png`);
    }
  });

  test('Search - zero coverage offers surveys and disables Aladin while offline', async ({ page, studio }) => {
    let captured: Record<string, unknown> | null = null;
    await page.route('**/api/coordinates/resolve?*', route => route.fulfill({ json: {
      name: 'RA 161.2600 Dec -59.8700', ra: 161.26, dec: -59.87,
    } }));
    await page.route('**/api/coordinates/coverage?*', route => route.fulfill({ json: {
      ...coverage, counts: { HST: 0, JWST: 0 }, instruments: [],
      frame: { width_arcmin: 10, height_arcmin: 5.625, aspect: '16:9', covering: 0 },
    } }));
    await page.route('**/api/status', route => route.fulfill({ json: {
      aladin: false, session: studio.session.id, job: null, edits: null,
    } }));
    await page.route('**/api/capture/coordinates', route => {
      captured = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ status: 409, json: { detail: 'Capture intercepted for test' } });
    });
    await page.goto(`${studio.origin}/#/new`);
    await page.getByRole('textbox', { name: 'Target name or coordinates' }).fill('161.26 -59.87');
    await page.getByRole('button', { name: 'Search', exact: true }).click();
    const callout = page.getByRole('status').filter({ hasText: 'No HST/JWST here' });
    await expect(callout).toContainText('No HST/JWST here');
    await expect(page.getByRole('button', { name: 'Open in Aladin', exact: true })).toBeDisabled();
    await expect(page.getByRole('button', { name: 'Capture from Aladin', exact: true })).toBeDisabled();
    for (const theme of ['dark', 'light']) {
      await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
      await screenshot(page, `zero-coverage-${theme}.png`);
    }
    await page.getByRole('button', { name: 'Use surveys', exact: true }).click();
    await expect.poll(() => captured?.surveys_only).toBe(true);
  });

  test('Aladin - New project uses the existing capture flow', async ({ page, studio }) => {
    let captured = false;
    await page.route('**/api/capture', route => {
      captured = true;
      return route.fulfill({ json: { id: 'aladin', kind: 'capture', state: 'running', session: studio.session.id } });
    });
    await page.getByRole('link', { name: 'NightBloom Home', exact: true }).click();
    await page.getByRole('button', { name: 'New project', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Capture from Aladin', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Capture from Aladin', exact: true }).click();
    await expect.poll(() => captured).toBe(true);
    await expect(page).toHaveURL(/\/sky$/);
  });

  test('Search - slow coverage updates in place and network failures remain actionable', async ({ page, studio }) => {
    let checks = 0;
    await page.route('**/api/coordinates/resolve?*', route => route.fulfill({ json: position }));
    await page.route('**/api/coordinates/coverage?*', route => {
      checks++;
      return route.fulfill({ json: checks === 1 ? { ...coverage, pending: true } :
        { ...coverage, warning: "MAST isn't responding. Try again in a few minutes.", counts: { HST: 0, JWST: 0 } } });
    });
    await page.goto(`${studio.origin}/#/new`);
    await page.getByRole('textbox', { name: 'Target name or coordinates' }).fill('M16');
    await page.getByRole('button', { name: 'Search', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText("MAST isn't responding");
    await expect(page.getByText('No HST/JWST here', { exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Capture', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Retry', exact: true }).click();
    await expect.poll(() => checks).toBeGreaterThan(2);
  });
});
