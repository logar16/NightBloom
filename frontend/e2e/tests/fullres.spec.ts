import { test, expect, editScreen, increment } from '../fixtures';

test('Full resolution - flushes edits, starts a job and downloads the completed export', async ({ page, studio }) => {
  const savedPath = 'E:\\exports\\fullres-mocked.png';
  let started = false, polls = 0;
  await page.route('**/api/export/full/plan', route => route.fulfill({ json: {
    width: 6555, height: 6698, preview_width: 1378, preview_height: 1408, estimated_s: 600,
  } }));
  await page.route('**/api/export/full', async route => {
    expect(route.request().postDataJSON()).toEqual({ format: 'png8', quality: 92, long_side: 4096 });
    started = true;
    await route.fulfill({ json: { id: 'native-export', session: studio.session.id, name: studio.session.name, kind: 'export-full', state: 'running',
      stage: 'reproject', progress: .1, message: 'Reprojecting F090W', log: [] } });
  });
  await page.route('**/api/job', route => route.fulfill({ json: {
    id: 'native-export', session: studio.session.id, name: studio.session.name, kind: 'export-full', state: ++polls > 1 ? 'done' : 'running',
    stage: 'write', progress: 1, message: 'Saved fullres-mocked.png', log: [],
    result: { path: savedPath, download_url: '/api/export/full/download/fullres-mocked.png' },
  } }));
  await page.route('**/api/export/full/download/fullres-mocked.png', route => route.fulfill({
    contentType: 'image/png', headers: { 'Content-Disposition': 'attachment; filename="fullres-mocked.png"' },
    body: Buffer.from('89504e470d0a1a0a', 'hex'),
  }));
  await editScreen(page);
  await increment(page, 'Exposure');
  await page.getByRole('button', { name: 'Export options', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Image…', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Export image' });
  await dialog.getByRole('radio', { name: /^4K/ }).check();
  await dialog.getByRole('radio', { name: 'PNG (8-bit)', exact: true }).check();
  const downloadPromise = page.waitForEvent('download');
  await dialog.getByRole('button', { name: 'Export', exact: true }).click();
  const download = await downloadPromise;
  expect(started).toBe(true);
  expect(studio.edits.at(-1)?.global.output.exposure).toBeCloseTo(.05);
  expect(download.suggestedFilename()).toBe('fullres-mocked.png');
  await expect(page.locator('#toast')).toContainText(`${studio.session.name} · export finished`);
});
