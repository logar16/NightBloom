import { test, expect } from '../fixtures';
import { mkdir } from 'node:fs/promises';
import path from 'node:path';

test('Storage - safe bulk selection, confirmation and both themes', async ({ page }) => {
  const references = { live: [], trash: [], backups: [], fixtures: [] };
  const snapshot = {
    total: 42_000_000_000,
    totals: { downloads: 30_000_000_000, aligned: 3_200_000_000, previews: 100_000_000,
      sessions: 5_800_000_000, trash: 900_000_000, backups: 1_000_000_000, fixtures: 1_000_000_000 },
    downloads: [
      { path: 'eagle/hst_f656n.fits', name: 'hst_f656n.fits', size: 1_500_000_000,
        instrument: 'WFC3/UVIS', filter: 'F656N', status: 'trash_only',
        references: { ...references, trash: [{ id: 'deleted', name: 'Eagle Nebula — first capture' }] } },
      { path: 'eagle/hst_f502n.fits', name: 'hst_f502n.fits', size: 900_000_000,
        instrument: 'WFC3/UVIS', filter: 'F502N', status: 'trash_only',
        references: { ...references, backups: [{ id: '_backup_eagle', name: 'Eagle Nebula recovery' }] } },
      { path: 'jwst_f200w.fits', name: 'jwst_f200w.fits', size: 700_000_000,
        instrument: 'NIRCAM', filter: 'F200W', status: 'orphan', references },
      { path: 'protected.fits', name: 'protected.fits', size: 2_000_000_000, status: 'in_use',
        references: { ...references, live: [{ id: 'live', name: 'Pillars of Creation' }] } },
    ],
    trash: [{ id: 'deleted-20261024T200000Z', name: 'Eagle Nebula — first capture',
      size: 900_000_000, date: '2026-10-24T20:00:00Z' }],
    backups: [{ id: '_backup_eagle', name: 'Eagle Nebula recovery', size: 1_000_000_000, date: '2026-10-20T20:00:00Z' }],
  };
  const requests: unknown[] = [];
  // All cleanup traffic is fulfilled here: the e2e downloads junction is never mutated.
  await page.route('**/api/storage', route => route.fulfill({ json: snapshot }));
  await page.route('**/api/storage/delete', async route => {
    requests.push(route.request().postDataJSON());
    snapshot.downloads = snapshot.downloads.filter(item => item.status !== 'trash_only');
    await route.fulfill({ json: { freed_bytes: 2_400_000_000 } });
  });
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('radio', { name: 'Dark', exact: true }).click();
  await page.getByRole('button', { name: 'Storage', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Storage', exact: true });
  const deleted = dialog.getByRole('checkbox', { name: 'Select all only in deleted projects' });
  await deleted.check();
  await expect(dialog.getByRole('button', { name: 'Delete selected (2 items · 2.4 GB)', exact: true })).toBeEnabled();
  await dialog.locator('summary').filter({ hasText: 'In use' }).click();
  const protectedRow = dialog.locator('.storage-row').filter({ hasText: 'protected.fits' });
  await expect(protectedRow.getByRole('checkbox')).toHaveCount(0);
  await dialog.locator('summary').filter({ hasText: 'In use' }).click();
  const screenshots = process.env.ASTROIMP_STORAGE_SCREENSHOTS;
  if (screenshots) {
    await mkdir(screenshots, { recursive: true });
    await dialog.screenshot({ path: path.join(screenshots, 'storage-dark.png') });
  }
  await page.keyboard.press('Escape');
  await expect(page.getByRole('button', { name: 'Settings', exact: true })).toBeFocused();
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('radio', { name: 'Light', exact: true }).click();
  await page.getByRole('button', { name: 'Storage', exact: true }).click();
  await deleted.check();
  if (screenshots) await dialog.screenshot({ path: path.join(screenshots, 'storage-light.png') });
  await dialog.getByRole('button', { name: /^Delete selected/ }).click();
  expect(requests).toHaveLength(0);
  await expect(dialog.getByText('Delete permanently? This cannot be undone.')).toBeVisible();
  await dialog.getByRole('button', { name: 'Delete permanently', exact: true }).click();
  await expect(dialog.getByRole('status')).toHaveText('Freed 2.4 GB');
  expect(requests).toEqual([{ downloads: ['eagle/hst_f656n.fits', 'eagle/hst_f502n.fits'],
    trash: [], backups: [], caches: [] }]);
  await expect(dialog.getByRole('checkbox', { name: 'Select all only in deleted projects' })).toBeDisabled();
});
