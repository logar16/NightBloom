import { test, expect as baseExpect, layerRows } from '../fixtures';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

test.skip(!process.env.ASTROIMP_PERF_STAGE, 'Opt-in frozen-fixture performance benchmark');
const expect = baseExpect.configure({ timeout: 120_000 });

for (let run = 0; run < 3; run++) test(`Performance - frozen M16 run ${run + 1}`, async ({ page, studio }) => {
  test.setTimeout(600_000);
  await page.addInitScript(({ halfResolution, sequential }) => {
    window.__astroimpPerf = true;
    window.__astroimpPerfSequential = sequential;
    const tasks: number[] = [];
    Object.assign(window, { __astroimpLongTasks: tasks });
    new PerformanceObserver(list => tasks.push(...list.getEntries().map(e => e.duration))).observe({ type: 'longtask', buffered: true });
    const arrays: number[] = [];
    Object.assign(window, { __astroimpTextureArrays: arrays });
    const storage = WebGL2RenderingContext.prototype.texStorage3D;
    WebGL2RenderingContext.prototype.texStorage3D = function(target, levels, format, w, h, depth) {
      arrays.push(w * h * depth * 2);
      return storage.call(this, target, levels, format, w, h, depth);
    };
    if (halfResolution) {
      const parameter = WebGL2RenderingContext.prototype.getParameter;
      WebGL2RenderingContext.prototype.getParameter = function(name: number) {
        return name === this.MAX_TEXTURE_SIZE ? 2048 : parameter.call(this, name);
      };
    }
  }, { halfResolution: !!process.env.ASTROIMP_PERF_PREVIEW, sequential: !!process.env.ASTROIMP_PERF_SEQUENTIAL });
  await page.reload();
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(layerRows(page)).toHaveCount(studio.initial.layers.length);
  const measures = () => page.evaluate(() => performance.getEntriesByType('measure').map(e => ({
    name: e.name.replace('astroimp:', ''), duration: e.duration,
  })));
  await expect.poll(async () => (await measures()).some(e => e.name === 'first-render')).toBe(true);
  await expect(page.locator('.layer-thumbnail img')).toHaveCount(studio.initial.layers.length);
  const load = await measures();
  const textureArrays = await page.evaluate(() => (window as unknown as { __astroimpTextureArrays: number[] }).__astroimpTextureArrays);
  const renderer = await page.locator('#composite').evaluate(canvas => {
    const gl = (canvas as HTMLCanvasElement).getContext('webgl2')!, debug = gl.getExtension('WEBGL_debug_renderer_info');
    return debug ? gl.getParameter(debug.UNMASKED_RENDERER_WEBGL) as string : 'unavailable';
  });
  if (process.env.ASTROIMP_PERF_PREVIEW) await expect(page.locator('#canvas-badge')).toContainText('Preview at ½ resolution');
  await page.evaluate(() => {
    performance.clearMeasures();
    (window as unknown as { __astroimpLongTasks: number[] }).__astroimpLongTasks.length = 0;
  });
  await page.getByRole('button', { name: 'Rotate (R)', exact: true }).click();
  const angle = page.getByRole('textbox', { name: 'Angle value', exact: true });
  await angle.fill('30'); await angle.press('Enter');
  await expect.poll(async () => (await measures()).filter(e => e.name === 'thumbnail-refresh').length).toBeGreaterThan(0);
  const rotation = await measures();
  const longTasks = await page.evaluate(() => [...(window as unknown as { __astroimpLongTasks: number[] }).__astroimpLongTasks]);
  await layerRows(page).first().getByText(studio.initial.layers[0]!.name, { exact: true }).click();
  await page.keyboard.press('b');
  await page.getByRole('textbox', { name: 'Size value', exact: true }).fill('20');
  await page.getByRole('textbox', { name: 'Size value', exact: true }).press('Enter');
  const bounds = (await page.locator('#composite').boundingBox())!;
  await page.evaluate(() => performance.clearMeasures());
  await page.mouse.move(bounds.x + bounds.width * .2, bounds.y + bounds.height * .5);
  await page.mouse.down();
  const pointerMoves = Number(process.env.ASTROIMP_PERF_POINTS ?? 100);
  for (let i = 0; i < pointerMoves; i++) {
    await page.mouse.move(bounds.x + bounds.width * (.2 + i / pointerMoves * .6), bounds.y + bounds.height * (.5 + .1 * Math.sin(i * .2)));
  }
  await page.mouse.up();
  const painting = await measures();
  const directory = process.env.ASTROIMP_PERF_DIR!;
  await mkdir(directory, { recursive: true });
  const file = path.join(directory, 'frontend.json');
  const data = await readFile(file, 'utf8').then(JSON.parse).catch(() => ({})) as Record<string, unknown[]>;
  const stage = process.env.ASTROIMP_PERF_STAGE!;
  if (run === 0) data[stage] = [];
  data[stage]!.push({ run: run + 1, renderer, pointerMoves, dimensions: studio.session.meta, layers: studio.initial.layers.length, textureArrays, load, rotation, longTasks, painting });
  await writeFile(file, JSON.stringify(data, null, 2));
});
