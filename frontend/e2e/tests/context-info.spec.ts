import { test, expect, editScreen, layerRows } from '../fixtures';
import type { ProbeResult } from '../../src/screens/edit/info';

test('Context menus - hide others isolates the target without deleting layers', async ({ page, studio, request }) => {
  await editScreen(page);
  await layerRows(page).first().click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Hide others', exact: true }).click();
  await expect.poll(async () => {
    const saved = await (await request.get(`${studio.origin}/api/edits`)).json();
    return saved.layers.filter((layer: { visible: boolean }) => layer.visible).map((layer: { id: string }) => layer.id);
  }).toEqual([studio.initial.layers[0]!.id]);
  await expect(layerRows(page)).toHaveCount(studio.initial.layers.length);
});

test('Info - hover probes live edits, context inspect pins contributors and selects their layer', async ({ page, studio, request }) => {
  await editScreen(page);
  const canvas = page.getByLabel('Astronomical composition', { exact: true });
  await canvas.focus(); await page.keyboard.press('i');
  await page.keyboard.press(']');
  const size = Number(await page.getByRole('slider', { name: 'Size', exact: true }).getAttribute('aria-valuenow'));
  // Probe must use the unsaved viewport state, not the last edit on disk.
  await page.route('**/api/edits', async route => {
    if (route.request().method() === 'PUT') await route.fulfill({ json: { revision: 1 } });
    else await route.fallback();
  });
  const exposure = page.getByRole('textbox', { name: 'Exposure value', exact: true });
  await exposure.fill('.7'); await exposure.press('Enter');
  const box = (await canvas.boundingBox())!, center = { x: box.width / 2, y: box.height / 2 };
  const response = page.waitForResponse(r => r.url().endsWith('/api/probe') && r.ok());
  await canvas.hover({ position: center });
  const returned = await response;
  const body = returned.request().postDataJSON() as {
    state: { layers: unknown[]; global: { output: { exposure: number } } }; radius: number;
  };
  expect(body.state.layers.length).toBeGreaterThan(0);
  expect(body.state.global.output.exposure).toBeCloseTo(.7);
  expect(body.radius).toBeCloseTo(size / 2);
  const result = await returned.json() as ProbeResult, info = page.getByTestId('pixel-inspector');
  await expect(info.locator('.pixel-contributor').first()).toHaveAttribute('data-layer', result.layers[0]!.id);
  await expect(info).toContainText(result.final_hex.toUpperCase());
  await canvas.click({ button: 'right', position: center });
  const pinnedResponse = page.waitForResponse(r => r.url().endsWith('/api/probe') && r.request().method() === 'POST');
  await page.getByRole('menuitem', { name: /^Inspect pixel here/ }).click();
  const pinned = await (await pinnedResponse).json() as ProbeResult;
  await expect(info).toHaveAttribute('data-pinned', 'true');
  await expect(info.locator('.pixel-contributor').first()).toHaveAttribute('data-layer', pinned.layers[0]!.id);
  await info.locator('.pixel-contributor').first().click();
  await expect(page.locator('.layer-row.active')).toHaveAttribute('data-layer', pinned.layers[0]!.id);
  const saved = await (await request.get(`${studio.origin}/api/edits`)).json();
  expect(saved.global.output.exposure).toBeCloseTo(0);
  await page.keyboard.press('Escape');
  await expect(info).toHaveCount(0);
});
