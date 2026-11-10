import { test, expect, openHomeProject } from '../fixtures';
import type { Job } from '../../src/api/types';

test.describe('Capture rotation', () => {
  test('Sky - explains the rotation blocker, prevents unsaved selections and unlocks after cancellation', async ({ page, studio, request }) => {
    const status = await (await request.get(`${studio.origin}/api/status`)).json();
    let started = false;
    let job: Job = { id: 'blocked-rotation', session: studio.session.id, name: 'Capture rotation',
      kind: 'view-rotation', state: 'running', stage: 'search', progress: 0,
      message: 'Waiting for astronomy archive', cancel_requested: false, log: [] };
    const writes: string[] = [];
    page.on('request', request => {
      if (new URL(request.url()).pathname === '/api/selection' && request.method() === 'PUT') writes.push(request.postData()!);
    });
    await page.route('**/api/status', route => route.fulfill({ json: started ? { ...status, job, jobs: [job] } : status }));
    await page.route('**/api/session/rotation', route => {
      started = true;
      return route.fulfill({ json: job });
    });
    await page.route('**/api/job', route => route.fulfill({ json: job }));
    await page.route('**/api/job/cancel?**', route => {
      expect(new URL(route.request().url()).searchParams.get('id')).toBe(job.id);
      job = { ...job, cancel_requested: true };
      return route.fulfill({ json: job });
    });
    await openHomeProject(page);
    await page.getByRole('button', { name: 'Sky', exact: true }).click();
    const build = page.getByRole('button', { name: 'Build', exact: true }).last();
    await expect(build).toBeEnabled();
    await page.getByRole('button', { name: 'Rotate view right 90°', exact: true }).click();
    await expect(build).toBeDisabled();
    await expect(build).toHaveAccessibleDescription(/capture rotation/i);
    await expect(page.getByRole('button', { name: /^Select (?!all)/ }).first()).toBeDisabled();
    await expect(page.getByRole('button', { name: 'Clear selection', exact: true })).toBeDisabled();
    await expect(page.getByRole('status').filter({ hasText: 'Waiting for astronomy archive' })).toContainText('Capture rotation');
    await page.getByRole('button', { name: 'Cancel Capture rotation', exact: true }).click();
    await expect(page.getByRole('status').filter({ hasText: 'Cancelling Capture rotation' })).toContainText('Cancelling');
    await page.getByRole('button', { name: /^Project jobs/ }).click();
    const jobLink = page.getByRole('link', { name: /Capture rotation/ });
    await expect(jobLink).toContainText('Cancelling');
    await expect(jobLink).toHaveAttribute('href', /\/sky$/);
    await page.getByRole('button', { name: /^Project jobs/ }).click();
    expect(writes).toEqual([]);
    job = { ...job, state: 'cancelled' };
    await expect(build).toBeEnabled();
    await expect(page.getByRole('button', { name: /^Select (?!all)/ }).first()).toBeEnabled();
    await expect(page.getByRole('spinbutton', { name: 'Capture rotation', exact: true })).toHaveValue('0');
  });

  test('Sky - progress lasts through the saved image and errors restore the controls', async ({ page, studio, request }) => {
    const original = await (await request.get(`${studio.origin}/api/session`)).json();
    const image = await (await request.get(`${studio.origin}/api/session/snapshot.jpg`)).body();
    let rotation = 0, fail = false;
    let releaseWrite!: () => void, releaseImage!: () => void;
    const writeReady = new Promise<void>(resolve => { releaseWrite = resolve; });
    const imageReady = new Promise<void>(resolve => { releaseImage = resolve; });
    await page.route('**/api/session', route => route.fulfill({
      json: { ...original, view: { ...original.view, rotation_deg: rotation } },
    }));
    await page.route('**/api/session/rotation', async route => {
      if (fail) return route.fulfill({ status: 503, json: { detail: 'Rotation service unavailable' } });
      await writeReady;
      rotation = route.request().postDataJSON().rotation_deg;
      return route.fulfill({ json: { id: 'rotation', session: studio.session.id, kind: 'view-rotation', state: 'done' } });
    });
    await page.route('**/api/session/snapshot.jpg?**', async route => {
      if (new URL(route.request().url()).searchParams.get('rotation') === '90') await imageReady;
      return route.fulfill({ contentType: 'image/jpeg', body: image });
    });
    await page.reload();
    await openHomeProject(page);
    await page.getByRole('button', { name: 'Sky', exact: true }).click();
    const progress = page.getByRole('status').filter({ hasText: 'Updating view…' });
    const input = page.getByRole('spinbutton', { name: 'Capture rotation', exact: true });
    try {
      await expect(progress).toHaveCount(0);
      await page.getByRole('button', { name: 'Rotate view right 90°', exact: true }).click();
      await expect(progress).toBeVisible();
      await expect(input).toBeDisabled();
      const requested = page.waitForRequest(request => new URL(request.url()).searchParams.get('rotation') === '90');
      releaseWrite();
      await requested;
      await expect(progress).toBeVisible();
      await expect(page.getByRole('button', { name: 'Adjust view tone', exact: true })).toBeDisabled();
      releaseImage();
      await expect(progress).toHaveCount(0);
      await expect(input).toBeEnabled();
      fail = true;
      await page.getByRole('button', { name: 'Rotate view left 90°', exact: true }).click();
      await expect(page.getByRole('status').filter({ hasText: 'Rotation service unavailable' })).toContainText('Rotation service unavailable');
      await expect(input).toHaveValue('90');
      await expect(input).toBeEnabled();
      await expect(progress).toHaveCount(0);
      await expect(page.getByRole('button', { name: 'Open project', exact: true })).toHaveCount(0);
      await page.getByRole('link', { name: 'NightBloom Home', exact: true }).click();
      await expect(page.getByRole('button', { name: 'Open project', exact: true })).toHaveCount(1);
    } finally { releaseWrite(); releaseImage(); }
  });

  test('Sky - compass, quarter turns, keyboard nudges and reset persist without a new capture', async ({ page, studio, request }) => {
    const original = await (await request.get(`${studio.origin}/api/session`)).json();
    let rotation = 0;
    const writes: number[] = [];
    await page.route('**/api/session', route => route.fulfill({
      json: { ...original, view: { ...original.view, rotation_deg: rotation }, rebuild_required: rotation !== 0 },
    }));
    await page.route('**/api/session/rotation', route => {
      expect(route.request().headers()['x-astroimp-session']).toBe(studio.session.id);
      rotation = route.request().postDataJSON().rotation_deg;
      writes.push(rotation);
      return route.fulfill({ json: { id: 'rotation', session: studio.session.id, kind: 'view-rotation', state: 'done' } });
    });
    await page.reload();
    await openHomeProject(page);
    await page.getByRole('button', { name: 'Sky', exact: true }).click();
    const input = page.getByRole('spinbutton', { name: 'Capture rotation', exact: true });

    await test.step('Quarter turn matches north right and east up', async () => {
      await page.getByRole('button', { name: 'Rotate view right 90°', exact: true }).click();
      await expect.poll(() => writes.at(-1)).toBe(90);
      await expect(input).toHaveValue('90');
      await expect(page.getByRole('img', { name: 'NightBloom compass, 90°', exact: true })).toHaveAttribute('data-angle', '90');
      await expect(input).toBeEnabled();
    });
    await test.step('Numeric and arrow-key changes keep tenth-degree precision', async () => {
      await input.fill('-30');
      await expect.poll(() => writes.at(-1)).toBe(-30);
      await expect(input).toBeEnabled();
      await input.focus();
      await input.press('ArrowRight');
      await expect.poll(() => writes.at(-1)).toBe(-29.9);
      await expect(input).toBeEnabled();
      await page.getByRole('button', { name: 'Rotate view left 90°', exact: true }).click();
      await expect.poll(() => writes.at(-1)).toBe(-119.9);
      await expect(input).toBeEnabled();
    });
    await test.step('Existing project requires a rebuild, then reset restores zero', async () => {
      await page.getByRole('button', { name: 'Build', exact: true }).first().click();
      await expect(page.getByText('View rotation changed · Rebuild required', { exact: true })).toHaveCount(1);
      await expect(page.getByRole('button', { name: 'Open editor', exact: true })).toHaveCount(0);
      await page.getByRole('button', { name: 'Sky', exact: true }).click();
      await page.getByRole('button', { name: 'Reset capture rotation', exact: true }).click();
      await expect.poll(() => writes.at(-1)).toBe(0);
      await expect(input).toHaveValue('0');
    });
  });
});
