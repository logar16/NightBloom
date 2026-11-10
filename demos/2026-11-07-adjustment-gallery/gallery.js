const scenes = document.querySelector('#scenes');
const title = document.querySelector('#image-title');
const sourceLabel = document.querySelector('#image-source');
const selectionStatus = document.querySelector('#selection-status');
const controls = [...document.querySelectorAll('.effect input')];
const wipe = document.querySelector('#wipe');
const failure = document.querySelector('#gallery-error');
let sceneDefs = [];
let currentSceneIdx = 0;
let revision = 0;
let mode = 'build';
const reference = document.querySelector('#reference');

function report(error) {
  failure.textContent = error instanceof Error ? error.message : String(error);
  failure.hidden = false;
  selectionStatus.textContent = 'Image unavailable';
}

async function fetchLocal(src) {
  const response = await fetch(`${src}?v=${Date.now()}`);
  if (!response.ok) throw Error(`Could not load ${src}: HTTP ${response.status}`);
  return response;
}

function defineScene(def) {
  sceneDefs.push(def);
}

function bits() {
  return visibleControls().reduce((value, control) => value | (control.checked ? Number(control.value) : 0), 0);
}

function visibleControls() {
  return controls.filter(control => mode === 'build' ? Number(control.value) < 16 : Number(control.value) === 16);
}

function updateScope() {
  document.querySelectorAll('[data-scope]').forEach(element => {
    element.hidden = element.dataset.scope !== mode;
  });
  document.querySelectorAll('[data-mode]').forEach(button => {
    button.setAttribute('aria-pressed', String(button.dataset.mode === mode));
  });
  reference.parentElement.hidden = mode !== 'build';
}

function updateWipe() {
  const value = Number(wipe.value);
  for (const def of sceneDefs) def.frame.style.setProperty('--wipe', `${value}%`);
  document.querySelector('#wipe-value').textContent = `${value} / ${100 - value}`;
}

async function render() {
  const ticket = ++revision;
  const def = sceneDefs[currentSceneIdx];
  if (!def) return;
  const selected = bits();
  const count = visibleControls().filter(control => control.checked).length;
  const before = def.frame.querySelector('.cmp-before');
  const after = def.frame.querySelector('.cmp-after');
  const baseline = mode === 'sky' || reference.value === 'built' ? def.baseline : def.frames['0'];
  const result = mode === 'sky' ? selected ? def.sky_auto : def.baseline : def.frames[String(selected)];
  if (!baseline || !result) throw Error('This adjustment comparison is missing.');
  failure.hidden = true;
  title.textContent = def.name;
  sourceLabel.textContent = def.source;
  before.alt = `${mode === 'sky' ? 'Original Sky display' : reference.value === 'built' ? 'Normal built source' : 'Uncorrected demo composite'}, ${def.name}`;
  after.alt = `Selected adjustments, ${def.name}`;
  def.frame.querySelector('figure').setAttribute('aria-label', `${def.name} comparison`);
  def.frame.querySelector('.cmp-label-before').textContent = mode === 'sky' ? 'ORIGINAL DISPLAY'
    : reference.value === 'built' ? 'NORMAL BUILT SOURCE' : 'UNCORRECTED DEMO';
  def.frame.querySelector('.cmp-label-after').textContent = mode === 'sky' ? `SKY AUTO ${count ? 'ON' : 'OFF'}`
    : `DEMO COMPOSITE · ${count} on`;
  selectionStatus.textContent = 'Loading image…';
  before.src = baseline;
  after.src = result;
  try {
    await Promise.all([before.decode(), after.decode()]);
  } catch (error) {
    if (ticket !== revision) return;
    throw error;
  }
  if (ticket === revision) selectionStatus.textContent = `${count} on`;
}

function showScene(index) {
  currentSceneIdx = index;
  sceneDefs.forEach((def, i) => {
    def.frame.hidden = i !== index;
    def.button.setAttribute('aria-pressed', String(i === index));
  });
  updateWipe();
  void render().catch(report);
}

async function init() {
  const manifest = await (await fetchLocal('manifest.json')).json();
  if (!Array.isArray(manifest.bases) || manifest.bases.length !== 3) throw Error('Invalid base image manifest');
  const nav = document.querySelector('#base-images');
  const provenance = document.querySelector('#provenance');
  for (const base of manifest.bases) {
    if (Object.keys(base.frames).length !== 16 || !base.baseline || !base.sky_auto) throw Error(`Missing comparisons for ${base.name}`);
    defineScene({ ...base, src: 'scenes/comparison.html' });
  }
  await Promise.all(sceneDefs.map(async (def, index) => {
    const html = await (await fetchLocal(def.src)).text();
    const frame = document.createElement('div');
    frame.className = 'scene-frame';
    frame.hidden = true;
    frame.innerHTML = html;
    def.frame = frame;
    const button = document.createElement('button');
    button.className = 'base-button';
    button.type = 'button';
    button.setAttribute('aria-label', `${def.name}, ${def.source}`);
    button.setAttribute('aria-pressed', 'false');
    const thumbnail = document.createElement('img');
    thumbnail.alt = '';
    thumbnail.src = def.thumbnail;
    const label = document.createElement('span');
    const name = document.createElement('strong');
    name.textContent = def.name;
    const source = document.createElement('small');
    source.textContent = def.source;
    label.append(name, source);
    button.append(thumbnail, label);
    button.addEventListener('click', () => showScene(index));
    def.button = button;
  }));
  for (const def of sceneDefs) {
    scenes.append(def.frame);
    nav.append(def.button);
    const name = document.createElement('dt');
    name.textContent = `${def.name} · ${def.source}`;
    const detail = document.createElement('dd');
    detail.textContent = def.provenance;
    provenance.append(name, detail);
  }
  controls.forEach(control => control.addEventListener('change', () => { void render().catch(report); }));
  document.querySelector('#all-off').addEventListener('click', () => {
    visibleControls().forEach(control => { control.checked = false; });
    void render().catch(report);
  });
  document.querySelector('#all-on').addEventListener('click', () => {
    visibleControls().forEach(control => { control.checked = true; });
    void render().catch(report);
  });
  document.querySelectorAll('[data-mode]').forEach(button => {
    button.addEventListener('click', () => {
      mode = button.dataset.mode;
      updateScope();
      void render().catch(report);
    });
  });
  reference.addEventListener('change', () => { void render().catch(report); });
  wipe.addEventListener('input', updateWipe);
  let dragging = false;
  const setWipe = event => {
    const rect = scenes.getBoundingClientRect();
    wipe.value = String(Math.max(0, Math.min(100, Math.round((event.clientX - rect.left) / rect.width * 100))));
    updateWipe();
  };
  scenes.addEventListener('pointerdown', event => {
    if (event.button !== 0) return;
    dragging = true;
    scenes.setPointerCapture(event.pointerId);
    setWipe(event);
  });
  scenes.addEventListener('pointermove', event => { if (dragging) setWipe(event); });
  scenes.addEventListener('pointerup', () => { dragging = false; });
  scenes.addEventListener('pointercancel', () => { dragging = false; });
  const about = document.querySelector('#about');
  document.querySelector('#about-open').addEventListener('click', () => about.showModal());
  document.querySelector('#about-close').addEventListener('click', () => about.close());
  updateScope();
  showScene(0);
}

void init().catch(report);
