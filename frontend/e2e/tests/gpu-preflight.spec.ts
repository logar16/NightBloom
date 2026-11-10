import { test, expect, editScreen, exportPreview } from '../fixtures';

test('GPU - context loss disables rendering and offers recovery', async ({ page }) => {
  await editScreen(page);
  await expect(page.locator('#render-message')).toHaveText('');
  await page.locator('#composite').evaluate(canvas => {
    (canvas as HTMLCanvasElement).getContext('webgl2')!.getExtension('WEBGL_lose_context')!.loseContext();
  });

  await expect(page.locator('#render-message')).toContainText('GPU context lost');
  await expect(page.getByRole('button', { name: 'Reload editor', exact: true })).toBeEnabled();
});

test('GPU - a half-resolution preview keeps preview PNG exports server-side at full size', async ({ page, studio }) => {
  await page.addInitScript(() => {
    const parameter = WebGL2RenderingContext.prototype.getParameter;
    WebGL2RenderingContext.prototype.getParameter = function(name: number) {
      return name === this.MAX_TEXTURE_SIZE ? 256 : parameter.call(this, name);
    };
  });
  await page.reload();
  await editScreen(page);
  await expect(page.locator('#canvas-badge')).toContainText('Preview at ½ resolution');
  await page.route('**/api/export/full', route => route.fulfill({
    status: 409, json: { detail: 'Export request intercepted by GPU contract test' },
  }));
  const request = page.waitForRequest(request => new URL(request.url()).pathname === '/api/export/full');
  await exportPreview(page);
  expect((await request).postDataJSON()).toMatchObject({
    format: 'png8', long_side: Math.max(studio.session.meta.width, studio.session.meta.height),
  });
  await expect(page.locator('#toast')).toContainText('Export request intercepted');
});
