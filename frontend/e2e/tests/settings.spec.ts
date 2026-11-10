import { test, expect } from '../fixtures';

test('Settings - saved theme is applied before React mounts', async ({ page }) => {
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('radio', { name: 'Light', exact: true }).click();
  await page.keyboard.press('Escape');
  await page.addInitScript(() => {
    const observer = new MutationObserver(() => {
      if (document.documentElement.dataset.theme) {
        (window as unknown as { firstTheme: string }).firstTheme = document.documentElement.dataset.theme;
        observer.disconnect();
      }
    });
    observer.observe(document, { subtree: true, attributes: true, attributeFilter: ['data-theme'] });
  });
  await page.reload();
  await expect.poll(() => page.evaluate(() => (window as unknown as { firstTheme: string }).firstTheme)).toBe('light');
});

test('Settings - System updates the active theme when the OS changes', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' });
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('radio', { name: 'System', exact: true }).click();
  await page.keyboard.press('Escape');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.emulateMedia({ colorScheme: 'dark' });
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});
