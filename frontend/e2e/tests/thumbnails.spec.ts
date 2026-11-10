import { test, expect, editScreen, layerRows } from '../fixtures';

test('Thumbnails - GPU images update selectively', async ({ page, studio }) => {
  await editScreen(page);
  const images = page.getByRole('img', { name: /layer thumbnail$/ });
  await expect(images).toHaveCount(studio.initial.layers.length);
  await expect.poll(() => images.evaluateAll(nodes => nodes.every(node => {
    const image = node as HTMLImageElement;
    if (!image.complete || !image.naturalWidth) return false;
    const canvas = document.createElement('canvas'); canvas.width = image.naturalWidth; canvas.height = image.naturalHeight;
    const ctx = canvas.getContext('2d')!; ctx.drawImage(image, 0, 0);
    const rgba = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
    const maxima = Array.from({ length: rgba.length / 4 }, (_, i) => Math.max(rgba[i * 4]!, rgba[i * 4 + 1]!, rgba[i * 4 + 2]!)).sort((a, b) => a - b);
    return maxima[Math.ceil(maxima.length * .995) - 1]! >= 120;
  }))).toBe(true);
  const first = images.first(), original = await first.getAttribute('src'), unchanged = await images.nth(1).getAttribute('src');
  await layerRows(page).first().click();
  await page.getByRole('button', { name: 'Tint', exact: true }).click();
  await page.getByRole('textbox', { name: 'Hex color', exact: true }).fill('#36c9df');
  await page.keyboard.press('Escape');
  await expect(first).not.toHaveAttribute('src', original!);
  await expect(images.nth(1)).toHaveAttribute('src', unchanged!);
});
