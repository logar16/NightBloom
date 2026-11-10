// Injected by `astroimp-desktop --headless-smoke` into the hidden WebView (never in normal runs).
// Drives the real, unchanged frontend: opens the first built project, waits for the composition
// canvas to hold real (varied) pixels, nudges the Exposure slider with the keyboard, waits for the
// debounced /api/edits autosave to agree, then reports through an intercepted custom-scheme
// navigation (astroimp-smoke://result?payload=...) that the shell cancels and handles itself.
(() => {
  'use strict';
  if (window !== window.top || window.__astroSmoke || !/^https?:$/.test(location.protocol)) return;
  window.__astroSmoke = true;

  const DEADLINE_MS = 24000; // the Rust watchdog (30 s) is only a backup for a wedged page
  const started = Date.now();
  const steps = [];
  const note = (step, extra) => {
    if (steps.length < 40) steps.push({ t: Date.now() - started, step, ...extra });
  };
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  let finished = false;

  // Evidence about whether a hidden WebView can paint: rAF is paused by Chromium for hidden pages.
  let frames = 0;
  const countFrames = () => { frames += 1; requestAnimationFrame(countFrames); };
  requestAnimationFrame(countFrames);

  const gl = () => {
    try {
      const probe = document.createElement('canvas').getContext('webgl2');
      const info = probe && probe.getExtension('WEBGL_debug_renderer_info');
      return probe
        ? { webgl2: true, renderer: info ? probe.getParameter(info.UNMASKED_RENDERER_WEBGL) : probe.getParameter(probe.RENDERER) }
        : { webgl2: false };
    } catch (error) {
      return { webgl2: false, error: String(error) };
    }
  };

  const finish = (ok, error, extra) => {
    if (finished) return;
    finished = true;
    const payload = {
      ok, error: error || null, elapsed_ms: Date.now() - started,
      visibility: document.visibilityState, frames, gl: gl(),
      ...extra, steps,
    };
    let text = JSON.stringify(payload);
    if (text.length > 12000) text = JSON.stringify({ ok, error: error || null, truncated: true });
    // The shell cancels this navigation (on_navigation returns false); nothing is ever loaded.
    location.href = 'astroimp-smoke://result?payload=' + encodeURIComponent(text);
  };

  const until = async (label, probe) => {
    for (;;) {
      const value = await probe();
      if (value) return value;
      if (Date.now() - started > DEADLINE_MS) throw new Error('Timed out waiting for ' + label);
      await sleep(100);
    }
  };

  // Samples the WebGL canvas through a small 2D copy (it is created with preserveDrawingBuffer).
  const canvasStats = () => {
    const canvas = document.querySelector('canvas[aria-label="Astronomical composition"]');
    if (!canvas || !canvas.width || !canvas.height) return null;
    const copy = document.createElement('canvas');
    copy.width = copy.height = 32;
    const context = copy.getContext('2d');
    context.drawImage(canvas, 0, 0, 32, 32);
    const pixels = context.getImageData(0, 0, 32, 32).data;
    let lit = 0, low = 255, high = 0;
    for (let i = 0; i < pixels.length; i += 4) {
      for (let c = 0; c < 3; c += 1) {
        const value = pixels[i + c];
        if (value > 16) lit += 1;
        low = Math.min(low, value);
        high = Math.max(high, value);
      }
    }
    return { width: canvas.width, height: canvas.height, lit, low, high };
  };

  const run = async () => {
    await until('the page body', () => document.body);
    const sessions = await (await fetch('/api/sessions')).json();
    const project = (Array.isArray(sessions) ? sessions : []).find((entry) => entry.built);
    if (!project) throw new Error('No built project in the data folder (' + (sessions || []).length + ' projects)');
    note('project', { id: project.id });
    const opened = await fetch('/api/sessions/' + encodeURIComponent(project.id) + '/open', { method: 'POST' });
    if (!opened.ok) throw new Error('Opening the project failed: HTTP ' + opened.status);
    location.hash = '#/p/' + encodeURIComponent(project.id) + '/edit';

    const pixels = await until('actual composition pixels', () => {
      const stats = canvasStats();
      // Real pixels: enough lit channel values and a real spread, not a uniform background.
      return stats && stats.lit > 100 && stats.high - stats.low > 24 ? stats : null;
    });
    note('canvas', pixels);

    const slider = await until('the Exposure slider', () => document.querySelector('[role="slider"][aria-label="Exposure"]'));
    const before = Number(slider.getAttribute('aria-valuenow'));
    const step = (type) => slider.dispatchEvent(new KeyboardEvent(type, { key: 'ArrowRight', code: 'ArrowRight', bubbles: true, cancelable: true }));
    slider.focus();
    step('keydown');
    step('keyup');
    note('exposure keys', { before });

    const after = await until('the slider value to change', () => {
      const value = Number(slider.getAttribute('aria-valuenow'));
      return value > before ? value : null;
    });
    const saved = await until('the autosaved exposure to match', async () => {
      const response = await fetch('/api/edits', { headers: { 'X-AstroImp-Session': project.id } });
      if (!response.ok) return null;
      const edits = await response.json();
      const value = edits && edits.global && edits.global.output && edits.global.output.exposure;
      return typeof value === 'number' && Math.abs(value - after) < 1e-6 ? value : null;
    });
    note('autosave', { slider: after, saved });

    // Real linked-core export, fetch-only (no anchors, no Save dialog): a tiny synthetic project
    // makes this about a second. Native size when the plan says it is safe, else the preview size.
    const headers = { 'X-AstroImp-Session': project.id };
    const planResponse = await fetch('/api/export/full/plan', { headers });
    if (!planResponse.ok) throw new Error('Export plan failed: HTTP ' + planResponse.status);
    const plan = await planResponse.json();
    const nativeLong = Math.max(plan.width, plan.height);
    const maxSafe = typeof plan.max_long_side === 'number' ? plan.max_long_side : nativeLong;
    const useNative = nativeLong <= maxSafe;
    const longSide = useNative ? nativeLong : Math.max(plan.preview_width, plan.preview_height);
    const expected = useNative ? [plan.width, plan.height] : [plan.preview_width, plan.preview_height];
    note('export plan', { native: [plan.width, plan.height], preview: [plan.preview_width, plan.preview_height], maxSafe, longSide });
    const exportStarted = Date.now();
    const started_job = await fetch('/api/export/full', {
      method: 'POST',
      headers: { ...headers, 'Content-Type': 'application/json' },
      body: JSON.stringify({ format: 'png16', long_side: longSide }),
    });
    if (!started_job.ok) throw new Error('Starting the export failed: HTTP ' + started_job.status + ' ' + (await started_job.text()).slice(0, 300));
    const job = await started_job.json();
    const finished_job = await until('the export job to finish', async () => {
      const response = await fetch('/api/job', { headers });
      if (!response.ok) throw new Error('Reading the job failed: HTTP ' + response.status);
      const next = await response.json();
      if (!next || next.id !== job.id) throw new Error('The export job is no longer available.');
      return next.state === 'running' || next.state === 'queued' ? null : next;
    });
    if (finished_job.state !== 'done' || !finished_job.result || !finished_job.result.download_url) {
      throw new Error('Export ended as ' + finished_job.state + ': ' + String(finished_job.error || 'no download').slice(0, 300));
    }
    const download = await fetch(finished_job.result.download_url, { headers });
    if (!download.ok) throw new Error('Downloading the export failed: HTTP ' + download.status);
    const bytes = new DataView(await download.arrayBuffer());
    const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if (bytes.byteLength < 33 || signature.some((value, index) => bytes.getUint8(index) !== value)
      || bytes.getUint32(12) !== 0x49484452) {
      throw new Error('The downloaded export is not a PNG (' + bytes.byteLength + ' bytes).');
    }
    const png = { width: bytes.getUint32(16), height: bytes.getUint32(20), depth: bytes.getUint8(24), color_type: bytes.getUint8(25) };
    const exported = {
      format: 'png16', long_side: longSide, kind: useNative ? 'native' : 'preview', max_long_side: plan.max_long_side ?? null,
      expected, job_state: finished_job.state, job_ms: Date.now() - exportStarted, bytes: bytes.byteLength,
      result: [finished_job.result.width, finished_job.result.height], png,
    };
    note('export', exported);
    if (png.width !== expected[0] || png.height !== expected[1]) {
      throw new Error('PNG is ' + png.width + 'x' + png.height + ', the plan chose ' + expected[0] + 'x' + expected[1] + '.');
    }
    if (png.depth !== 16) throw new Error('PNG bit depth is ' + png.depth + ', expected 16.');
    finish(true, null, { project: project.id, pixels, exposure: { before, after, saved }, export: exported });
  };

  const sample = () => { try { return canvasStats(); } catch (error) { return { error: String(error) }; } };
  run().catch((error) => finish(false, String((error && error.message) || error), { pixels: sample() }));
})();
