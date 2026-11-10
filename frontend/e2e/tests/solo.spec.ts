import { test, expect, editScreen, layerRows, exportPreview } from '../fixtures';

test('Solo - latching changes the view but exports the full composite without saving edits', async ({ page, studio }) => {
  await editScreen(page);
  await layerRows(page).first().click();
  const pixels = () => page.locator('#composite').evaluate(node => (node as HTMLCanvasElement).toDataURL());
  const full = await pixels();
  const exportPNG = async () => {
    const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
    await exportPreview(page);
    const png = (await posted).postDataBuffer()!;
    await download;
    return png;
  };
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'solo-parity.png' } }));
  const expected = await exportPNG(), saved = studio.edits.length;
  await layerRows(page).first().getByRole('button', { name: 'Show only this layer (Q)', exact: true }).click();
  await expect.poll(pixels).not.toBe(full);
  expect(await exportPNG()).toEqual(expected);
  expect(studio.edits).toHaveLength(saved);
});
