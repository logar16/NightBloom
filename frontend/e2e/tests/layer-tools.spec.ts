import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect, editScreen, exportPreview } from '../fixtures';
import { type EditorState, type ProjectMeta } from '../../src/editor/state';

const screenshotDir = process.env.ASTROIMP_LAYER_SCREENSHOTS;
async function screenshot(page: Page, name: string) {
  if (!screenshotDir) return;
  await mkdir(screenshotDir, { recursive: true });
  for (const theme of ['dark', 'light']) {
    await page.evaluate(t => { document.documentElement.dataset.theme = t; }, theme);
    await page.screenshot({ path: path.join(screenshotDir, `${name}-${theme}.png`) });
  }
}

async function difference(page: Page, gpu: Buffer, cpu: Buffer) {
  return page.evaluate(async ({ gpu, cpu }) => {
    const decode = async (encoded: string) => {
      const image = await createImageBitmap(await (await fetch(`data:image/png;base64,${encoded}`)).blob());
      const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height;
      const context = canvas.getContext('2d')!; context.drawImage(image, 0, 0); image.close();
      return context.getImageData(0, 0, canvas.width, canvas.height).data;
    };
    const a = await decode(gpu), b = await decode(cpu);
    let sum = 0, max = 0, count = 0, aboveOne = 0;
    for (let i = 0; i < a.length; i++) if (i % 4 !== 3) {
      const d = Math.abs(a[i] - b[i]); sum += d; max = Math.max(max, d); count++;
      if (d > 1) aboveOne++;
    }
    return { mean: sum / count, max, aboveOne: aboveOne / count, corner: Array.from(a.slice(0, 3)) };
  }, { gpu: gpu.toString('base64'), cpu: cpu.toString('base64') });
}

test('Layer tools - properties headings clearly separate tone, mask and edge in both themes', async ({ page }) => {
  await editScreen(page);
  const tone = page.getByRole('heading', { name: 'Tone', exact: true });
  const mask = page.getByRole('heading', { name: 'Mask', exact: true });
  await tone.scrollIntoViewIfNeeded();
  await screenshot(page, 'properties-tone');
  const section = page.locator('.property-section').filter({ has: mask });
  await section.getByRole('combobox', { name: 'Source', exact: true }).click();
  await page.getByRole('option').nth(1).click();
  await mask.scrollIntoViewIfNeeded();
  await screenshot(page, 'properties-mask-edge');
  for (const width of [1440, 1100]) {
    await page.setViewportSize({ width, height: 860 });
    expect(await page.locator('#properties').evaluate(e => e.scrollWidth <= e.clientWidth)).toBe(true);
    await page.getByRole('heading', { name: 'Output · all layers', exact: true }).scrollIntoViewIfNeeded();
    await expect(page.getByRole('slider', { name: 'Exposure', exact: true })).toBeVisible();
  }
});

test('Layer tools - invert keeps gaps empty and matches NumPy', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { meta: ProjectMeta; edits: EditorState };
  const state = structuredClone(session.edits), target = state.layers[0];
  for (const layer of state.layers) layer.visible = layer.id === target.id;
  Object.assign(target, { stretch: 'asinh', k: 15, intensity: 1, invert: true,
    transform: { x: .07, y: -.04, scale: .85, rotation: 13 } });
  state.selectedLayer = target.id;
  await request.put(`${studio.origin}/api/edits`, { data: state });
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'layer-tools.png' } }));
  await page.reload(); await editScreen(page);
  const invert = page.getByRole('checkbox', { name: 'Invert', exact: true });
  await expect(invert).toBeChecked();
  await screenshot(page, 'invert-after');
  const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
  await exportPreview(page);
  const png = (await posted).postDataBuffer()!; await download;
  const report = await (await request.get(`${studio.origin}/api/analyze?max_side=3072`)).json() as { preview: string };
  const result = await difference(page, png, await readFile(report.preview));
  await test.info().attach('invert-parity', { body: JSON.stringify(result), contentType: 'application/json' });
  expect(result.mean).toBeLessThan(.5);
  // Hardware bilinear sampling differs most at transformed, one-pixel footprint boundaries.
  expect(result.aboveOne).toBeLessThan(.01);
  expect(result.corner).toEqual([0, 0, 0]);
  await invert.uncheck(); await screenshot(page, 'invert-before');
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(invert).toBeChecked();
});

test('Layer tools - curves edit by pointer and keyboard, undo and match NumPy', async ({ page, studio, request }) => {
  await editScreen(page);
  const curve = page.getByRole('group', { name: 'Tone curve', exact: true });
  await curve.scrollIntoViewIfNeeded();
  await curve.click({ position: { x: 70, y: 50 } });
  await expect(curve.getByRole('slider')).toHaveCount(3);
  const point = curve.getByRole('slider', { name: 'Curve point 2', exact: true });
  await point.focus(); await point.press('ArrowUp');
  await expect(point).toBeFocused();
  await screenshot(page, 'curve-editor');
  await point.press('Delete');
  await expect(curve.getByRole('slider')).toHaveCount(2);
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(curve.getByRole('slider')).toHaveCount(3);
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { edits: EditorState };
  const state = structuredClone(session.edits), target = state.layers[0];
  for (const layer of state.layers) layer.visible = layer.id === target.id;
  Object.assign(target, { invert: true, intensity: .8,
    curve: [{ x: 0, y: .06 }, { x: .27, y: .15 }, { x: .65, y: .85 }, { x: 1, y: .94 }] });
  await request.put(`${studio.origin}/api/edits`, { data: state });
  await page.reload(); await editScreen(page);
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'curve-parity.png' } }));
  const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
  await exportPreview(page);
  const png = (await posted).postDataBuffer()!; await download;
  const report = await (await request.get(`${studio.origin}/api/analyze?max_side=3072`)).json() as { preview: string };
  const result = await difference(page, png, await readFile(report.preview));
  await test.info().attach('curve-parity', { body: JSON.stringify(result), contentType: 'application/json' });
  expect(result.mean).toBeLessThan(.5);
  expect(result.max).toBeLessThan(6);
});

test('Layer tools - detail sharpening and local contrast match NumPy', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { edits: EditorState };
  const state = structuredClone(session.edits), target = state.layers[0];
  for (const layer of state.layers) layer.visible = layer.id === target.id;
  Object.assign(target, { stretch: 'asinh', k: 15, intensity: 1, opacity: 1, color: '#dddddd' });
  state.selectedLayer = target.id;
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'detail-parity.png' } }));
  for (const radius of [2, 24]) for (const amount of [0, 1.5]) {
    target.detail = { amount, radius };
    await request.put(`${studio.origin}/api/edits`, { data: state });
    await page.reload(); await editScreen(page);
    await page.getByRole('slider', { name: 'Amount', exact: true }).scrollIntoViewIfNeeded();
    await screenshot(page, `detail-${amount}-radius-${radius}`);
    const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
    await exportPreview(page);
    const png = (await posted).postDataBuffer()!; await download;
    if (screenshotDir) await writeFile(path.join(screenshotDir, `detail-${amount}-radius-${radius}-composite.png`), png);
    const report = await (await request.get(`${studio.origin}/api/analyze?max_side=3072`)).json() as { preview: string };
    const result = await difference(page, png, await readFile(report.preview));
    await test.info().attach(`detail-${amount}-${radius}`, { body: JSON.stringify(result), contentType: 'application/json' });
    expect(result.mean).toBeLessThan(.5);
    expect(result.max).toBeLessThan(6);
  }
  const amount = page.getByRole('slider', { name: 'Amount', exact: true });
  await amount.focus(); await amount.press('ArrowRight');
  await expect(amount).toHaveAttribute('aria-valuenow', '1.55');
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(amount).toHaveAttribute('aria-valuenow', '1.5');
});

test('Layer tools - source painting and live detail updates match NumPy', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { edits: EditorState; meta: ProjectMeta };
  const state = structuredClone(session.edits), target = state.layers[0];
  for (const layer of state.layers) layer.visible = layer.id === target.id;
  Object.assign(target, { mode: 'tint', color: '#c08040', blend: 'normal', stretch: 'asinh', k: 10,
    intensity: .8, opacity: .7, invert: false, detail: { amount: 1.5, radius: 4 },
    curve: [{ x: 0, y: .06 }, { x: .3, y: .2 }, { x: 1, y: 1 }] });
  target.brush.strokes = [
    { mode: 'white', size: 80, hardness: .5, strength: .75, points: [[session.meta.width * .4, session.meta.height * .45]] },
    { mode: 'black', size: 40, hardness: .5, strength: .5, points: [[session.meta.width * .4, session.meta.height * .45]] },
  ];
  state.selectedLayer = target.id;
  await request.put(`${studio.origin}/api/edits`, { data: state });
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'source-paint-parity.png' } }));
  await page.reload(); await editScreen(page);
  await page.keyboard.press('b');
  await page.getByRole('radio', { name: 'White', exact: true }).click();
  const canvas = page.getByLabel('Astronomical composition', { exact: true });
  const bounds = (await canvas.boundingBox())!;
  await page.mouse.click(bounds.x + bounds.width * .6, bounds.y + bounds.height * .55);
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers[0]?.brush.strokes.length).toBe(3);
  const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
  await exportPreview(page);
  const png = (await posted).postDataBuffer()!; await download;
  const report = await (await request.get(`${studio.origin}/api/analyze?max_side=3072`)).json() as { preview: string };
  const result = await difference(page, png, await readFile(report.preview));
  await test.info().attach('source-paint-parity', { body: JSON.stringify(result), contentType: 'application/json' });
  expect(result.mean).toBeLessThan(.5);
  expect(result.max).toBeLessThan(6);
});

test('Layer tools - frozen M16 detail GPU cost', async ({ page, studio, request }) => {
  test.skip(!process.env.ASTROIMP_DETAIL_PERF, 'Opt-in synchronized GPU timing');
  test.setTimeout(600_000);
  await page.addInitScript(() => { window.__astroimpPerf = true; });
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { edits: EditorState; meta: ProjectMeta };
  const state = structuredClone(session.edits);
  const results = [];
  for (const count of [1, 3]) for (const radius of [2, 24]) {
    await page.goto('about:blank');
    state.layers.forEach((l, i) => { l.visible = i < count; l.detail = { amount: i < count ? 1.5 : 0, radius }; });
    await request.put(`${studio.origin}/api/edits`, { data: state });
    await page.goto(studio.url);
    await page.getByRole('button', { name: 'Edit', exact: true }).click();
    // Load the full-size frozen fixture before timing; regular UI tests use downsampled assets.
    await expect(page.getByTestId('layer-row')).not.toHaveCount(0, { timeout: 120_000 });
    await expect(page.getByRole('slider', { name: 'Amount', exact: true })).toHaveAttribute('aria-valuenow', '1.5', { timeout: 120_000 });
    const measures = () => page.evaluate(() => performance.getEntriesByName('astroimp:recompose').map(e => e.duration));
    await expect.poll(async () => (await measures()).length).toBeGreaterThan(0);
    const cold = (await measures())[0];
    await page.evaluate(() => performance.clearMeasures('astroimp:recompose'));
    const exposure = page.getByRole('slider', { name: 'Exposure', exact: true });
    await exposure.scrollIntoViewIfNeeded(); await exposure.focus();
    for (let i = 0; i < 10; i++) await exposure.press(i % 2 ? 'ArrowLeft' : 'ArrowRight');
    const samples = (await measures()).sort((a, b) => a - b);
    const radiusInput = page.getByRole('textbox', { name: 'Radius value', exact: true });
    await radiusInput.scrollIntoViewIfNeeded();
    const before = (await measures()).length;
    await radiusInput.fill(String(radius + 1)); await radiusInput.press('Enter');
    await expect.poll(async () => (await measures()).length).toBeGreaterThan(before);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    const radiusChange = (await measures()).slice(before);
    expect(await page.locator('#composite').evaluate(c => (c as HTMLCanvasElement).getContext('webgl2')!.getError())).toBe(0);
    results.push({ layers: count, radius, cold, median: samples[Math.floor(samples.length / 2)],
      radiusChange: Math.max(...radiusChange), samples });
  }
  const renderer = await page.locator('#composite').evaluate(c => {
    const gl = (c as HTMLCanvasElement).getContext('webgl2')!, ext = gl.getExtension('WEBGL_debug_renderer_info');
    return ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) as string : 'unknown';
  });
  const report = { dimensions: [session.meta.width, session.meta.height], renderer, results };
  await test.info().attach('detail-timing', { body: JSON.stringify(report, null, 2), contentType: 'application/json' });
  if (screenshotDir) await writeFile(path.join(screenshotDir, 'detail-perf.json'), JSON.stringify(report, null, 2));
});

test('Layer tools - crop draws, resizes, moves and clears as undoable edits', async ({ page }) => {
  await editScreen(page);
  const crop = page.getByRole('button', { name: 'Crop layer', exact: true });
  await crop.click();
  await expect(crop).toHaveAttribute('aria-pressed', 'true');
  const overlay = page.getByRole('group', { name: 'Layer crop overlay', exact: true });
  const bounds = (await overlay.boundingBox())!;
  await page.mouse.move(bounds.x + bounds.width*.3, bounds.y + bounds.height*.2);
  await page.mouse.down();
  await page.mouse.move(bounds.x + bounds.width*.7, bounds.y + bounds.height*.8, { steps: 6 });
  await page.mouse.up();
  await expect(page.getByRole('button', { name: 'Resize layer crop bottom right', exact: true })).toHaveCount(1);
  const handle = page.getByRole('button', { name: 'Resize layer crop bottom right', exact: true });
  const before = await handle.getAttribute('x');
  await handle.focus(); await handle.press('ArrowLeft');
  expect(await handle.getAttribute('x')).not.toBe(before);
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(handle).toHaveAttribute('x', before!);
  const move = page.getByRole('button', { name: 'Move layer crop', exact: true });
  await move.focus(); await move.press('ArrowRight');
  await page.getByRole('button', { name: 'Ellipse', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Ellipse', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await screenshot(page, 'crop-overlay');
  await page.keyboard.press('Escape');
  await screenshot(page, 'crop-composite');
  await page.getByRole('button', { name: 'Remove layer crop', exact: true }).click();
  await expect(overlay).toHaveCount(0);
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(overlay).toHaveCount(1);
});

test('Layer tools - rectangle and ellipse crop parity stays in canvas space', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { edits: EditorState; meta: ProjectMeta };
  const state = structuredClone(session.edits);
  const source = session.meta.layers.find(l => l.collection === 'HST')!;
  const target = state.layers.find(l => l.source === source.key)!;
  state.selectedLayer = target.id;
  for (const layer of state.layers) layer.visible = layer.id === target.id;
  Object.assign(target, { stretch: 'linear', k: 15, intensity: 1, invert: false,
    transform: { x: .07, y: -.04, scale: 1.1, rotation: 13 }, edge: { feather: 12, irregularity: 0 } });
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'clip-parity.png' } }));
  for (const shape of ['rect', 'ellipse'] as const) {
    await page.goto('about:blank');
    target.clip = { x: .24, y: .2, w: .5, h: .6, feather: .2, shape };
    await request.put(`${studio.origin}/api/edits`, { data: state });
    await page.goto(studio.url); await editScreen(page);
    const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
    await exportPreview(page);
    const png = (await posted).postDataBuffer()!; await download;
    const report = await (await request.get(`${studio.origin}/api/analyze?max_side=3072`)).json() as { preview: string };
    const result = await difference(page, png, await readFile(report.preview));
    await test.info().attach(`clip-${shape}`, { body: JSON.stringify(result), contentType: 'application/json' });
    expect(result.mean).toBeLessThan(.5);
    expect(result.max).toBeLessThan(6);
  }
});
