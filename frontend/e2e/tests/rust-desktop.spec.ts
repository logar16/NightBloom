import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';

const origin = process.env.ASTROIMP_RUST_ORIGIN;
const session = 'rust-validation';

test.skip(!origin, 'Requires an isolated Rust server and the prepare_rust_fixture.py project');

test('Rust desktop milestone - existing project renders, autosaves, reopens and exports real images', async ({ page, request }) => {
  const headers = { 'X-AstroImp-Session': session };
  await test.step('Open the existing project and render science pixels', async () => {
    const opened = await request.post(`${origin}/api/sessions/${session}/open`);
    expect(opened.ok()).toBeTruthy();
    await page.goto(`${origin}/#/p/${session}/edit`);
    await expect(page).toHaveTitle(/ · Edit — NightBloom$/);
    const canvas = page.getByLabel('Astronomical composition', { exact: true });
    await expect.poll(() => canvas.evaluate(node => {
      const copy = document.createElement('canvas');
      copy.width = copy.height = 32;
      const context = copy.getContext('2d')!;
      context.drawImage(node as HTMLCanvasElement, 0, 0, 32, 32);
      const pixels = context.getImageData(0, 0, 32, 32).data;
      return Array.from(pixels).filter((value, index) => index % 4 !== 3 && value > 16).length;
    })).toBeGreaterThan(100);
  });

  await test.step('Persist an actual editor adjustment and retain it after reload', async () => {
    const exposure = page.getByRole('slider', { name: 'Exposure', exact: true });
    await expect(exposure).toHaveAttribute('aria-valuenow', '0');
    await exposure.focus();
    await exposure.press('ArrowRight');
    await expect.poll(async () => {
      const response = await request.get(`${origin}/api/edits`, { headers });
      expect(response.ok()).toBeTruthy();
      return (await response.json()).global.output.exposure as number;
    }).toBeCloseTo(.05);
    await page.reload();
    await expect(page.getByRole('slider', { name: 'Exposure', exact: true }))
      .toHaveAttribute('aria-valuenow', '0.05');
  });

  await test.step('Download a Rust-rendered 16-bit image and a genuinely native-resolution image', async () => {
    for (const choice of [
      { resolution: /^Preview/, format: 'PNG (16-bit)', width: 64, height: 48, depth: 16 },
      { resolution: /^Native/, format: 'PNG (8-bit)', width: 128, height: 96, depth: 8 },
    ]) {
      await page.getByRole('button', { name: 'Export options', exact: true }).click();
      await page.getByRole('menuitem', { name: 'Image…', exact: true }).click();
      const dialog = page.getByRole('dialog', { name: 'Export image' });
      await dialog.getByRole('radio', { name: choice.resolution }).check();
      await dialog.getByRole('radio', { name: choice.format, exact: true }).check();
      const downloadPromise = page.waitForEvent('download');
      await dialog.getByRole('button', { name: 'Export', exact: true }).click();
      const download = await downloadPromise;
      expect(await download.failure()).toBeNull();
      const downloadedPath = await download.path();
      expect(downloadedPath).not.toBeNull();
      const png = await readFile(downloadedPath!);
      expect(png.subarray(0, 8).toString('hex')).toBe('89504e470d0a1a0a');
      expect(png.readUInt32BE(16)).toBe(choice.width);
      expect(png.readUInt32BE(20)).toBe(choice.height);
      expect(png[24]).toBe(choice.depth);
    }
  });
});

test('Rust desktop export - memory limits disable unsupported native sizes without relabeling them', async ({ page, request }) => {
  await request.post(`${origin}/api/sessions/${session}/open`);
  await page.route('**/api/export/full/plan', async route => {
    const response = await route.fetch();
    await route.fulfill({ response, json: { ...await response.json(), max_long_side: 64 } });
  });
  await page.goto(`${origin}/#/p/${session}/edit`);
  await page.getByRole('button', { name: 'Export options', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Image…', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Export image' });
  await expect(dialog.getByRole('radio', { name: /^Native/ })).toBeDisabled();
  await dialog.getByRole('radio', { name: /^Preview/ }).check();
  await dialog.getByRole('radio', { name: 'PNG (16-bit)', exact: true }).check();
  await expect(dialog.getByRole('button', { name: 'Export', exact: true })).toBeEnabled();
});

test('Native library export - saves in the project by default and offers explicit Export as', async ({ page, request }) => {
  await page.addInitScript(() => Object.defineProperty(window, '__TAURI_INTERNALS__', { value: {} }));
  const fileActions: { project: string; action: string; filename?: string }[] = [];
  // Exercise the UI request, never launch the user's image viewer or Explorer during a test.
  await page.route('**/api/desktop/projects/**', async route => {
    const parts = new URL(route.request().url()).pathname.split('/');
    expect(route.request().method()).toBe('POST');
    fileActions.push({ project: decodeURIComponent(parts.at(-2)!), action: parts.at(-1)!,
      filename: route.request().postDataJSON()?.filename });
    await route.fulfill({ json: { opened: true } });
  });
  await request.post(`${origin}/api/sessions/${session}/open`);
  await page.goto(`${origin}/#/p/${session}/edit`);
  await page.getByRole('button', { name: 'Open project folder', exact: true }).click();
  expect(fileActions).toEqual([{ project: session, action: 'folder', filename: undefined }]);
  const downloads: string[] = [];
  page.on('download', download => downloads.push(download.suggestedFilename()));
  for (const { format, depth, saveAs } of [
    { format: 'PNG (8-bit)', depth: 8, saveAs: false },
    { format: 'PNG (8-bit)', depth: 8, saveAs: true },
    { format: 'PNG (16-bit)', depth: 16, saveAs: false },
    { format: 'PNG (16-bit)', depth: 16, saveAs: true },
  ]) {
    await page.getByRole('button', { name: 'Export options', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Image…', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Export image' });
    await dialog.getByRole('radio', { name: /^Preview/ }).check();
    await dialog.getByRole('radio', { name: format, exact: true }).check();
    await expect(dialog.getByLabel('Estimated file size')).toHaveText(/^≈[\d.]+ (KB|MB)$/);
    const downloadPromise = saveAs ? page.waitForEvent('download') : null;
    const started = page.waitForResponse(response => response.url().endsWith(depth === 8 ? '/api/export' : '/api/export/full')
      && response.request().method() === 'POST');
    await dialog.getByRole('button', { name: saveAs ? 'Export as…' : 'Export', exact: true }).click();
    const result = await (await started).json();
    let saved: string = result.path;
    if (depth === 16) await expect.poll(async () => {
      const response = await request.get(`${origin}/api/job`, { headers: { 'X-AstroImp-Session': session } });
      const latest = await response.json();
      if (latest.id === result.id && latest.state === 'done') saved = latest.result.path;
      return latest.id === result.id ? latest.state : 'other';
    }).toBe('done');
    await expect(page.getByRole('button', { name: 'Export options', exact: true })).toBeEnabled();
    expect(saved.split(/[\\/]/).slice(-3, -1)).toEqual([session, 'exports']);
    const png = await readFile(saved);
    expect(png[24]).toBe(depth);
    await page.getByRole('button', { name: 'Open image', exact: true }).click();
    await expect.poll(() => fileActions.at(-1)).toEqual({
      project: session, action: 'open-image', filename: saved.split(/[\\/]/).at(-1),
    });
    await page.getByRole('button', { name: 'Show in folder', exact: true }).click();
    await expect.poll(() => fileActions.at(-1)).toEqual({
      project: session, action: 'reveal', filename: saved.split(/[\\/]/).at(-1),
    });
    if (downloadPromise) expect(await (await downloadPromise).failure()).toBeNull();
    expect(downloads).toHaveLength((depth === 16 ? 1 : 0) + (saveAs ? 1 : 0));
  }
});

test('Project workspace - rename persists across screens and Home starts new projects', async ({ page, request }) => {
  const headers = { 'X-AstroImp-Session': session };
  await request.post(`${origin}/api/sessions/${session}/open`);
  const { name } = await (await request.get(`${origin}/api/session`, { headers })).json() as { name: string };
  await page.goto(`${origin}/#/p/${session}/sky`);
  await expect(page.getByRole('button', { name, exact: true })).toHaveText(name);
  await page.keyboard.press('F2');
  const input = page.getByRole('textbox', { name: 'Project name', exact: true });
  await input.fill(`${name} renamed`);
  await input.press('Enter');
  await expect.poll(async () => (await (await request.get(`${origin}/api/session`, { headers })).json()).name)
    .toBe(`${name} renamed`);
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await page.getByRole('button', { name: `${name} renamed`, exact: true }).click();
  await input.fill(name);
  await input.press('Enter');
  await expect.poll(async () => (await (await request.get(`${origin}/api/session`, { headers })).json()).name).toBe(name);
  await page.getByRole('link', { name: 'NightBloom Home', exact: true }).click();
  await page.getByRole('button', { name: 'New project', exact: true }).click();
  await expect(page).toHaveURL(/#\/new$/);
  await expect(page.getByRole('textbox', { name: 'Target name or coordinates' })).toBeEditable();
});
