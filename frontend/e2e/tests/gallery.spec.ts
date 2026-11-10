import { execFileSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { test, expect, editScreen, exportPreview } from '../fixtures';
import { root, runDir, sessionIds } from '../setup';
import type { GalleryProject } from '../../src/api/types';
import type { EditorState } from '../../src/editor/state';
import { readExportLink } from '../../src/api/png-links';

test('Home exports - browses without loading a project and copies or saves the original image', async ({ page, studio, request }) => {
  execFileSync('uv', ['run', '--no-sync', 'python', path.join(root, 'frontend', 'e2e', 'prepare-gallery.py'), runDir],
    { cwd: root, windowsHide: true });
  await page.addInitScript(() => {
    const copies: { width: number; height: number; type: string }[] = [];
    Object.assign(window, { galleryCopies: copies });
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
      write: async (items: ClipboardItem[]) => {
        const blob = await items[0].getType('image/png');
        const image = await createImageBitmap(blob);
        copies.push({ width: image.width, height: image.height, type: blob.type }); image.close();
      },
    } });
  });

  await page.reload();
  const projectLoads: string[] = [];
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => {
    const url = new URL(request.url());
    if (/^\/api\/session$|^\/api\/project\/|^\/api\/sessions\/[^/]+\/open$/.test(url.pathname))
      projectLoads.push(url.pathname);
  });
  const projects = await (await request.get(`${studio.origin}/api/gallery`)).json() as GalleryProject[];
  await expect(page.getByRole('button', { name: /^View exports from / })).toHaveCount(2);
  const project = projects.find(item => item.id === sessionIds[0])!;
  await page.getByRole('button', { name: `View exports from ${project.name}`, exact: true }).click();
  const viewer = page.getByRole('dialog', { name: 'Export viewer' });
  await expect(viewer.getByRole('heading', { name: 'native-768.png', exact: true })).toHaveText('native-768.png');
  await expect(viewer.getByRole('button', { name: /^View (native|colour|preview)/ }))
    .toHaveCount(3);
  const labels = await viewer.getByRole('button', { name: /^View (native|colour|preview)/ })
    .evaluateAll(buttons => buttons.map(button => button.getAttribute('aria-label')));
  expect(labels).toEqual(['View preview.png', 'View native-768.png', 'View colour-16.tiff']);
  await expect(viewer.getByRole('button', { name: 'Edit exported version', exact: true })).toBeDisabled();
  await expect(viewer.getByText('2 / 3', { exact: true })).toBeVisible();
  await viewer.press('ArrowRight');
  await expect(viewer.getByRole('heading', { name: 'colour-16.tiff', exact: true })).toHaveText('colour-16.tiff');
  await viewer.getByRole('button', { name: 'Copy image', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('Copied colour-16.tiff');
  const copies = await page.evaluate(() => (window as typeof window & {
    galleryCopies: { width: number; height: number; type: string }[];
  }).galleryCopies);
  expect(copies).toEqual([{ width: project.images[1].width, height: project.images[1].height, type: 'image/png' }]);
  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
      write: async () => { throw new Error('Clipboard permission denied'); },
    } });
  });
  await page.route('**/api/gallery/**?view=clipboard&*', route =>
    route.fulfill({ status: 500, json: { detail: 'The source image could not be decoded.' } }));
  const rejectedCopy = page.waitForResponse(response => response.url().includes('view=clipboard') && response.status() === 500);
  await viewer.getByRole('button', { name: 'Copy image', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('Clipboard permission denied');
  await (await rejectedCopy).finished();
  const downloading = page.waitForEvent('download');
  await viewer.getByRole('button', { name: 'Save original image', exact: true }).click();
  const download = await downloading;
  expect(await readFile((await download.path())!)).toEqual(await readFile(
    path.join(runDir, 'data', 'sessions', project.id, 'exports', 'earlier', 'colour-16.tiff')));
  await page.evaluate(() => { Object.assign(window, { __TAURI_INTERNALS__: {} }); });
  const reveal = page.waitForRequest(request => request.url().endsWith(`/api/desktop/projects/${project.id}/reveal`));
  await page.route('**/api/desktop/projects/*/reveal', route => route.fulfill({ json: { opened: true } }));
  await viewer.press('ArrowLeft'); await viewer.press('ArrowRight');
  await viewer.getByRole('button', { name: 'Show image in folder', exact: true }).click();
  expect((await reveal).postDataJSON()).toEqual({ filename: 'earlier/colour-16.tiff' });
  await viewer.press('Escape');
  await expect(viewer).toHaveCount(0);
  expect(projectLoads).toEqual([]);
  expect(errors).toEqual([]);
});

test('Home exports - an exported GPU look reopens while keeping newer edits as a return checkpoint', async ({ page, studio, request }) => {
  await editScreen(page);
  const checkpointRequest = page.waitForRequest(request => new URL(request.url()).pathname === '/api/export/checkpoint');
  const stored = page.waitForResponse(response => new URL(response.url()).pathname === '/api/export'
    && response.request().method() === 'POST');
  const downloading = page.waitForEvent('download');
  await exportPreview(page);
  const sent = (await checkpointRequest).postDataJSON() as { state: EditorState };
  const response = await stored;
  expect(response.status()).toBe(200);
  const { path: image } = await response.json() as { path: string };
  const history = JSON.parse(await readFile(path.join(path.dirname(image), '.history.json'), 'utf8')) as
    Record<string, { checkpoint: string }>;
  const checkpoint = history[path.basename(image)].checkpoint;
  const saved = JSON.parse(await readFile(path.join(runDir, 'data', 'sessions',
    studio.session.id, 'export-checkpoints', `${checkpoint}.json`), 'utf8')) as { state: EditorState };
  const { checkpoints: _checkpoints, ...snapshot } = sent.state;
  expect(saved.state).toEqual(snapshot);
  const downloaded = await readFile((await (await downloading).path())!);
  expect(downloaded).toEqual(await readFile(image));
  expect(await readExportLink(new Blob([downloaded]))).toEqual({
    version: 1, project: studio.session.id, checkpoint,
  });
  const saving = page.waitForResponse(response => new URL(response.url()).pathname === '/api/edits'
    && response.request().method() === 'PUT');
  const exposure = page.getByRole('slider', { name: 'Exposure', exact: true });
  await exposure.focus(); await exposure.press('ArrowRight');
  const newer = (await saving).request().postDataJSON() as EditorState;
  expect(newer.global.output.exposure).not.toBe(snapshot.global.output.exposure);
  await page.getByRole('link', { name: 'NightBloom Home', exact: true }).click();
  const cover = page.getByRole('article', { name: studio.session.name, exact: true });
  await cover.getByRole('button', { name: `View exports from ${studio.session.name}`, exact: true }).focus();
  await cover.getByRole('button', { name: 'Open current project', exact: true }).click();
  await expect(page.getByRole('slider', { name: 'Exposure', exact: true }))
    .toHaveAttribute('aria-valuenow', String(newer.global.output.exposure));
  await page.goto(`${studio.origin}/`);
  await page.getByRole('button', { name: `View exports from ${studio.session.name}`, exact: true }).click();
  await page.getByRole('button', { name: `View ${path.basename(image)}`, exact: true }).click();
  await page.getByRole('dialog', { name: 'Export viewer' })
    .getByRole('button', { name: 'Edit exported version', exact: true }).click();
  await expect(page.getByRole('slider', { name: 'Exposure', exact: true }))
    .toHaveAttribute('aria-valuenow', String(snapshot.global.output.exposure));
  const reopened = await (await request.get(`${studio.origin}/api/edits`)).json() as EditorState;
  const returning = reopened.checkpoints!.find(point => point.exportCheckpoint)!;
  expect(returning.state.global.output.exposure).toBe(newer.global.output.exposure);
  await page.getByRole('button', { name: 'Checkpoints', exact: true }).click();
  await page.getByRole('button', { name: returning.name, exact: true }).click();
  await expect(page.getByRole('slider', { name: 'Exposure', exact: true }))
    .toHaveAttribute('aria-valuenow', String(newer.global.output.exposure));

  await test.step('Paste a copied PNG from Home without needing a focused project control', async () => {
    await page.goto(`${studio.origin}/#/home`);
    await expect(page.getByRole('heading', { name: 'Home', exact: true })).toHaveText('Home');
    await page.evaluate(bytes => {
      const clipboard = new DataTransfer();
      clipboard.items.add(new File([new Uint8Array(bytes)], 'renamed-art.png', { type: 'image/png' }));
      document.body.dispatchEvent(new ClipboardEvent('paste', { clipboardData: clipboard, bubbles: true, cancelable: true }));
    }, Array.from(downloaded));
    await expect(page.getByRole('slider', { name: 'Exposure', exact: true }))
      .toHaveAttribute('aria-valuenow', String(snapshot.global.output.exposure));
    const restored = await (await request.get(`${studio.origin}/api/edits`)).json() as EditorState;
    expect(restored.checkpoints!.find(point => point.exportCheckpoint === returning.exportCheckpoint)!
      .state.global.output.exposure).toBe(newer.global.output.exposure);
  });

  await test.step('Reject metadata-stripped images without touching working edits, then open a renamed linked file', async () => {
    await page.goto(`${studio.origin}/`);
    const before = await (await request.get(`${studio.origin}/api/edits`)).json() as EditorState;
    const size = downloaded.readUInt32BE(33);
    const stripped = Buffer.concat([downloaded.subarray(0, 33), downloaded.subarray(45 + size)]);
    await page.getByLabel('Exported PNG file', { exact: true })
      .setInputFiles({ name: 'stripped.png', mimeType: 'image/png', buffer: stripped });
    await expect(page.getByRole('status')).toContainText('no saved-look link');
    expect(await (await request.get(`${studio.origin}/api/edits`)).json()).toEqual(before);
    await page.getByLabel('Exported PNG file', { exact: true })
      .setInputFiles({ name: 'copied-elsewhere.png', mimeType: 'image/png', buffer: downloaded });
    await expect(page.getByRole('slider', { name: 'Exposure', exact: true }))
      .toHaveAttribute('aria-valuenow', String(snapshot.global.output.exposure));
  });

  await test.step('Drop an exported PNG onto Home', async () => {
    await page.goto(`${studio.origin}/`);
    const files = await page.evaluateHandle(bytes => {
      const transfer = new DataTransfer();
      transfer.items.add(new File([new Uint8Array(bytes)], 'dropped.png', { type: 'image/png' }));
      return transfer;
    }, Array.from(downloaded));
    await page.getByRole('region', { name: 'Home', exact: true }).dispatchEvent('drop', { dataTransfer: files });
    await files.dispose();
    await expect(page.getByRole('slider', { name: 'Exposure', exact: true }))
      .toHaveAttribute('aria-valuenow', String(snapshot.global.output.exposure));
  });
});
