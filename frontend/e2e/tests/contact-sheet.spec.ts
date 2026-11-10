import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { runDir, sessionIds } from '../setup';
import { defaultState, type ProjectMeta } from '../../src/editor/state';

test('Contact sheet - keyboard menu flushes edits and downloads a real saved PNG', async ({ page, request }) => {
  test.setTimeout(60000);
  const { origin } = JSON.parse(await readFile(path.join(runDir, 'server.json'), 'utf8')) as { origin: string };
  await request.post(`${origin}/api/sessions/${sessionIds[0]}/open`);
  const session = await (await request.get(`${origin}/api/session`)).json() as { meta: ProjectMeta };
  await request.put(`${origin}/api/edits`, { data: defaultState(session.meta) });
  // No routes: the menu, persistence, renderer and download all use the isolated real server.
  await page.goto(origin);
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const options = page.getByRole('button', { name: 'Export options', exact: true });
  await expect(options).toBeEnabled();
  const exposure = page.getByRole('slider', { name: 'Exposure', exact: true });
  await exposure.focus();
  await exposure.press('ArrowRight');
  const expectedExposure = Number(await exposure.getAttribute('aria-valuenow'));
  await options.focus();
  await options.press('ArrowDown');
  await expect(page.getByRole('menuitem', { name: 'Image…', exact: true })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(page.getByRole('menuitem', { name: 'Contact sheet', exact: true })).toBeFocused();
  const responsePromise = page.waitForResponse(response =>
    response.url().endsWith('/api/export/contact-sheet') && response.request().method() === 'POST');
  const downloadPromise = page.waitForEvent('download');
  await page.keyboard.press('Enter');
  const response = await responsePromise, download = await downloadPromise;
  expect(response.status()).toBe(200);
  expect(download.suggestedFilename()).toMatch(/^contact-sheet-.*\.png$/);
  const savedPath = response.headers()['x-export-path'];
  expect(savedPath).toContain(path.join(runDir, 'data', 'sessions'));
  const bytes = await readFile(savedPath);
  expect(bytes.subarray(0, 8).toString('hex')).toBe('89504e470d0a1a0a');
  expect((await readFile((await download.path())!)).equals(bytes)).toBe(true);
  const savedEdits = await (await request.get(`${origin}/api/edits`)).json();
  expect(savedEdits.global.output.exposure).toBeCloseTo(expectedExposure);
  await expect(page.getByText(`Contact sheet saved: ${savedPath}`, { exact: true })).toBeVisible();
});
