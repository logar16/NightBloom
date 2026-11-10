import { test, expect, openHomeProject } from '../fixtures';
import type { Job, PlanFile } from '../../src/api/types';

test('Build - distinguishes upstream estimates, unknown sizes, and actual transferred bytes', async ({ page, studio }) => {
  const files: PlanFile[] = studio.session.meta.layers.slice(0, 3).map((layer, index) => ({
    obs_id: layer.obs_id, filename: `${layer.obs_id}.fits`, filter: layer.filter,
    size_bytes: [4_000_000, 80_000_000, null][index] ?? null, cached: index === 1,
    transfer_bytes: [180_000_000, 0, null][index] ?? null,
    transfer_estimated: index === 0, transferred_bytes: 0,
    transfer_note: index === 0 ? 'Estimated internet transfer for native source tiles.' : 'Transfer metadata unavailable.',
  }));
  files.push({ obs_id: 'survey-mdw', filename: 'mdw.fits', filter: 'Hα', size_bytes: null,
    transfer_bytes: 0, transferred_bytes: 0, source_fields: Array.from({ length: 7 }, (_, index) => ({
      name: `Field ${2000 + index}`, size_bytes: 64_016_640, cached: true,
    })) });
  let job: Job | null = null;
  await page.route('**/api/build/plan*', route => route.fulfill({ json: {
    files, layer_count: 4, total_bytes: 84_000_000, cached_bytes: 80_000_000,
  } }));
  await page.route('**/api/job*', route => route.fulfill({ json: job }));
  await page.route('**/api/status*', async route => {
    const response = await route.fetch();
    await route.fulfill({ response, json: { ...await response.json(), job, jobs: job ? [job] : [] } });
  });
  await openHomeProject(page);
  await page.getByRole('button', { name: 'Build', exact: true }).click();
  await expect(page.getByText('Download 2 images (~180.0 MB + unknown) · Prepare 1 image (cached sources) · 1 cached (80.0 MB) · 4 images → 4 layers', { exact: true })).toHaveCount(1);
  await expect(page.getByText('7 fields · ~64.0 MB/field · 7 cached', { exact: true })).toHaveCount(1);
  await expect(page.getByText('to prepare', { exact: true })).toHaveCount(1);
  await expect(page.getByTitle('Estimated internet transfer for native source tiles.', { exact: true })).toHaveText('~180.0 MB');
  await expect(page.getByTitle('Cached FITS file size', { exact: true })).toHaveText('80.0 MB');
  await expect(page.getByTitle('Transfer metadata unavailable.', { exact: true })).toHaveText('Size pending');
  job = {
    id: 'transfer', session: studio.session.id, name: studio.session.name, kind: 'build',
    state: 'running', stage: 'download', progress: .3, message: 'Downloading source tiles', log: [],
    files: files.map((file, index) => ({
      ...file, state: index === 1 ? 'cached' : index === 0 ? 'downloading' : index === 3 ? 'assembling' : 'done',
      transfer_bytes: index === 2 ? 35_000_000 : file.transfer_bytes,
      transferred_bytes: [90_000_000, 0, 35_000_000, 0][index],
      done_bytes: [0, 80_000_000, 3_000_000][index],
    })),
  };
  await expect(page.getByText(/^125\.0 MB of ~215\.0 MB/)).toHaveCount(1);
  await expect(page.getByRole('button', { name: 'Project jobs (1), download 58%', exact: true })).toHaveCount(1);
  await page.getByRole('button', { name: 'Project jobs (1), download 58%', exact: true }).click();
  await expect(page.getByRole('link', { name: new RegExp(`${studio.session.name} 58% build download`) })).toHaveCount(1);
  await page.keyboard.press('Escape');
  job = { ...job, state: 'done', stage: 'done', progress: 1,
    files: job.files!.map((file, index) => ({
      ...file, state: index === 1 ? 'cached' : 'done', transfer_estimated: false,
      transfer_bytes: index === 0 ? 165_000_000 : file.transfer_bytes,
      transferred_bytes: index === 0 ? 165_000_000 : file.transferred_bytes,
      done_bytes: index === 0 ? 4_000_000 : file.done_bytes,
    })),
  };
  await expect(page.getByText(/^200\.0 MB of 200\.0 MB/)).toHaveCount(1);
  job = { ...job, state: 'running', stage: 'align', phase: 'align', progress: .5, eta_s: 36,
    files: job.files!.map(file => ({ ...file, cached: true, state: 'cached', transfer_bytes: 0 })),
    steps: [
      { key: 'first', label: 'Align F502N', state: 'running', progress: .5, eta_s: 6 },
      { key: 'next', label: 'Align F547M', state: 'pending', progress: 0, eta_s: 30 },
    ],
  };
  await expect(page.getByText('No internet transfer needed · ~36 sec', { exact: true })).toHaveCount(1);
  await expect(page.getByText('~6 sec', { exact: true })).toHaveCount(1);
  await page.getByRole('button', { name: 'Project jobs (1), align 50%', exact: true }).click();
  await expect(page.getByRole('link', { name: new RegExp(`${studio.session.name} 50% build align · ~36 sec`) })).toHaveCount(1);
});
