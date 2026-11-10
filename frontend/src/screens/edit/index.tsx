import { StatusBar } from '../../StatusBar';
import { useCallback, useEffect, useRef, useState } from 'react';
import { ChevronDown, Download, Image as ImageIcon, Keyboard, LayoutPanelTop, LoaderCircle, Redo2, TriangleAlert, Undo2 } from 'lucide-react';
import { useSession, useStatus, toast, refreshSession } from '../../api/hooks';
import { api, projectHeaders, projectUrl } from '../../api/client';
import { deliverExport, isDesktop, saveDownload } from '../../api/desktop';
import { attachExportLink } from '../../api/png-links';
import { IconButton, Popover } from '../../ui';
import { Compositor } from '../../editor/compositor';
import { editorStore, useEditor } from '../../editor/store';
import { snapshot, type Crop, type ProjectMeta } from '../../editor/state';
import { CanvasView, type CanvasHandle, type Tool } from './canvas';
import { CaptionDialog, makeCaption } from './caption';
import { Checkpoints, LayerPanel, SpectralRail } from './panels';
import { ActiveLayer, Properties, type PropertyTab } from './properties';
import { ToolOptions } from './options';
import { useEditorPersistence } from './session';
import { useMarkersSession } from './markers';
import './edit.css';
import { defaultBrush } from './brush';
import { soloStore, useSolo } from '../../editor/solo';
import { ImageExportDialog, useFullResolutionExport } from './fullres';
import { loadInfoSize } from './info';
import { applyViewCrop, viewFrame } from '../../editor/view';

function LoadedEditor({ sessionId, name, meta, tool, setToolValue, propertyTab, setPropertyTab }: {
  sessionId: string; name: string; meta: ProjectMeta; tool: Tool; setToolValue: (tool: Tool) => void;
  propertyTab: PropertyTab; setPropertyTab: (tab: PropertyTab) => void;
}) {
  const historyIndex = useEditor(s => s.historyIndex), historyLength = useEditor(s => s.history.length);
  useMarkersSession(sessionId, `${meta.built_at}:${meta.layers.map(l => l.key).join(',')}`);
  const { flush, saveError } = useEditorPersistence(sessionId);
  const handle = useRef<CanvasHandle | null>(null);
  const [brush, setBrush] = useState(defaultBrush), [inverted, setInverted] = useState(false);
  const [infoSize, setInfoSize] = useState(loadInfoSize);
  useEffect(() => {
    try { localStorage.setItem('astroimp.info.size', String(infoSize)); }
    catch { /* Size still persists while the editor is open. */ }
  }, [infoSize]);
  const [spacePan, setSpacePan] = useState(false), [shortcutsOpen, setShortcutsOpen] = useState(false);
  const shortcutsAnchor = useRef<HTMLButtonElement>(null);
  const solo = useSolo();
  useEffect(() => { soloStore.getState().exit(); return () => soloStore.getState().exit(); }, [sessionId]);
  const [aspect, setAspect] = useState(0), [pending, setPending] = useState<Crop | null>(null);
  const [compositor, setCompositor] = useState<Compositor | null>(null), [caption, setCaption] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false), [sheetRendering, setSheetRendering] = useState(false);
  const [exportMenu, setExportMenu] = useState(false);
  const [imageExport, setImageExport] = useState(false);
  const fullResolution = useFullResolutionExport(imageExport, flush);
  const crop = useEditor(s => s.state!.global.crop);
  const rotation = useEditor(s => s.state!.global.rotation);
  const frame = viewFrame(meta, { crop, rotation });
  const exportAnchor = useRef<HTMLButtonElement>(null), exportMenuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!exportMenu) return;
    const frame = requestAnimationFrame(() =>
      exportMenuRef.current?.querySelector<HTMLButtonElement>('[role=menuitem]')?.focus());
    return () => cancelAnimationFrame(frame);
  }, [exportMenu]);
  const [status, setStatus] = useState({ hint: 'WebGL2', pixel: '', zoom: '100%' });
  const onStatus = useCallback((change: Partial<typeof status>) => setStatus(s => ({ ...s, ...change })), []);
  const setTool = useCallback((value: Tool) => { setToolValue(value); setPending(null); }, [setToolValue]);
  const resetCrop = useCallback(() => {
    setPending(null);
    editorStore.getState().change(s => { s.global.crop = { x: 0, y: 0, w: 1, h: 1 }; });
    handle.current?.fit();
  }, []);
  const applyCrop = useCallback(() => {
    if (!pending) return;
    editorStore.getState().change(s => { s.global.crop = applyViewCrop(pending, s.global.rotation); });
    setPending(null); handle.current?.fit();
  }, [pending]);
  useEffect(() => {
    const release = (e: Event) => {
      if (!(e instanceof KeyboardEvent) || e.code === 'Space') setSpacePan(false);
      if (!(e instanceof KeyboardEvent) || e.key === 'Alt') setInverted(false);
      if (!(e instanceof KeyboardEvent) || e.key.toLowerCase() === 'q') soloStore.getState().release();
    };
    const listener = (e: KeyboardEvent) => {
      if (e.key === 'Alt') setInverted(true);
      if (!e.defaultPrevented && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's'
        && !document.querySelector('dialog[open]')) {
        e.preventDefault(); editorStore.getState().createCheckpoint();
        void flush().then(() => toast('Checkpoint saved')).catch((error: Error) => toast(error.message)); return;
      }
      if (e.defaultPrevented || (e.target instanceof Element
        && e.target.closest('input:not([type=checkbox]),textarea,select,[role=combobox],[contenteditable=true],.ui-popover')) || document.querySelector('dialog[open]')) return;
      const key = e.key.toLowerCase(), actions = editorStore.getState();
      if (key === 'escape' && shortcutsOpen) { setShortcutsOpen(false); return; }
      if ((e.ctrlKey || e.metaKey) && key === 'd' && !e.altKey) {
        e.preventDefault(); if (!e.repeat) actions.duplicateSelection(); return;
      }
      if ((e.ctrlKey || e.metaKey) && ['z', 'y'].includes(key)) {
        e.preventDefault(); if (key === 'y' || e.shiftKey) actions.redo(); else actions.undo(); return;
      }
      if (!e.ctrlKey && !e.metaKey && !e.altKey) {
        if (e.code === 'Space') { e.preventDefault(); setSpacePan(true); return; }
        if (key === '?') { e.preventDefault(); setShortcutsOpen(open => !open); return; }
        if (key === 'delete' || key === 'backspace') {
          e.preventDefault(); if (!e.repeat) actions.deleteSelection(); return;
        }
        if (key === 'q' && !e.repeat) {
          e.preventDefault(); soloStore.getState().hold(actions.selected); return;
        }
        const keys: Record<string, Tool> = { h: 'pan', v: 'layer', c: 'crop', r: 'rotate', m: 'mark', b: 'brush', i: 'info' };
        if (keys[key]) setTool(keys[key]);
        if ((tool === 'brush' || tool === 'info') && ['[', ']'].includes(key)) {
          e.preventDefault();
          if (tool === 'info') setInfoSize(size => Math.max(1, Math.min(200,
            key === '[' ? Math.min(size - 1, Math.round(size / 1.2)) : Math.max(size + 1, Math.round(size * 1.2)))));
          else setBrush(b => ({ ...b, size: Math.max(1, Math.min(1600, Math.round(b.size * (key === '[' ? 1 / 1.2 : 1.2)))) }));
        }
        if (key === 'f' && e.shiftKey) { e.preventDefault(); document.getElementById('fullscreen')?.click(); return; }
        if (key === 'f' || key === '0') { e.preventDefault(); handle.current?.fit(); }
        if (key === '1') { e.preventDefault(); handle.current?.actualSize(); }
      }
      if (e.key === 'Enter' && tool === 'crop') applyCrop();
      if (e.key === 'Escape') { soloStore.getState().exit(); setTool('pan'); }
    };
    document.addEventListener('keydown', listener);
    document.addEventListener('keyup', release); window.addEventListener('blur', release);
    return () => {
      document.removeEventListener('keydown', listener); document.removeEventListener('keyup', release);
      window.removeEventListener('blur', release);
    };
  }, [tool, applyCrop, flush, setTool, shortcutsOpen]);
  const measurePreviewPNG = useCallback(async () => {
    const comp = handle.current?.compositor;
    const state = editorStore.getState().state;
    if (!comp?.ready || !state) throw Error('The composition is not ready to measure.');
    comp.compose(state);
    return (await comp.exportPNG()).size;
  }, []);
  async function exportPNG(saveAs = false) {
    const comp = handle.current?.compositor;
    if (!comp?.ready) return;
    setExporting(true);
    try {
      await flush();
      const source = editorStore.getState();
      const state = snapshot(source.state!);
      const checkpoint = await api<{ checkpoint: string }>('/export/checkpoint', 'POST',
        { state, meta: source.meta }, sessionId);
      comp.compose(state);
      const blob = await attachExportLink(await comp.exportPNG(),
        { version: 1, project: sessionId, checkpoint: checkpoint.checkpoint });
      const header = new DataView(await blob.slice(16, 24).arrayBuffer());
      const filename = `${name.replace(/[^\w.-]+/g, '-').replace(/^[-.]+|[-.]+$/g, '') || 'nightbloom'}-${header.getUint32(0)}x${header.getUint32(4)}.png`;
      if (!isDesktop()) saveDownload(blob, filename);
      const response = await fetch('/api/export', {
        method: 'POST', headers: { ...projectHeaders(sessionId), 'Content-Type': 'image/png',
          'X-Name': filename, 'X-Export-Checkpoint': checkpoint.checkpoint }, body: blob,
      });
      if (!response.ok) throw Error(isDesktop() ? 'PNG could not be saved to the library.' : 'PNG downloaded; server copy could not be saved.');
      let exportPath: string | undefined;
      if (isDesktop()) {
        const saved: unknown = await response.json();
        if (!saved || typeof saved !== 'object' || !('path' in saved) || typeof saved.path !== 'string')
          throw Error('The server did not return the saved PNG path.');
        const storedName = saved.path.split(/[\\/]/).at(-1);
        if (!storedName) throw Error('The server returned an invalid PNG path.');
        exportPath = saved.path;
        deliverExport(projectUrl(`/api/export/full/download/${encodeURIComponent(storedName)}`, sessionId), filename, saveAs);
      }
      refreshSession();
      toast('PNG exported', isDesktop() ? sessionId : undefined, exportPath);
    } catch (error) { toast(error instanceof Error ? error.message : String(error)); }
    finally { setExporting(false); }
  }
  async function exportContactSheet() {
    setExporting(true); setSheetRendering(true);
    try {
      await flush();
      const response = await fetch('/api/export/contact-sheet', { method: 'POST', headers: projectHeaders(sessionId) });
      if (!response.ok) throw Error('Contact sheet could not be exported.');
      const savedPath = response.headers.get('X-Export-Path');
      const filename = savedPath?.split(/[\\/]/).at(-1) || 'contact-sheet.png';
      saveDownload(await response.blob(), filename);
      toast(`Contact sheet saved${savedPath ? `: ${savedPath}` : ''}`);
    } catch (error) { toast(error instanceof Error ? error.message : String(error)); }
    finally { setExporting(false); setSheetRendering(false); }
  }
  function chooseExport(sheet: boolean) {
    setExportMenu(false); exportAnchor.current?.focus();
    if (sheet) void exportContactSheet(); else setImageExport(true);
  }
  return <section id="edit" className="screen active">
    <div className="editor-toolbar">
      <div className="tool-group">
        <IconButton id="undo" icon={Undo2} title="Undo (Ctrl+Z)" disabled={historyIndex <= 0}
          onClick={() => editorStore.getState().undo()} />
        <IconButton id="redo" icon={Redo2} title="Redo (Ctrl+Shift+Z)" disabled={historyIndex >= historyLength - 1}
          onClick={() => editorStore.getState().redo()} />
        {saveError && <span className="save-warning" role="img" aria-label="Autosave failed"
          title="Autosave failed. Your next edit will retry."><TriangleAlert /></span>}
        <Checkpoints />
        <button ref={shortcutsAnchor} className={`icon ${shortcutsOpen ? 'active' : ''}`}
          title="Keyboard shortcuts (?)" aria-label="Keyboard shortcuts (?)" aria-expanded={shortcutsOpen}
          aria-controls={shortcutsOpen ? 'editor-shortcuts' : undefined}
          onClick={() => setShortcutsOpen(open => !open)}><Keyboard /></button>
        {shortcutsOpen && <Popover anchor={shortcutsAnchor} onClose={() => setShortcutsOpen(false)} className="shortcut-popover">
          <table id="editor-shortcuts" aria-label="Editor keyboard shortcuts"><tbody>
            {[
              ['H / V', 'Pan / Move layer'], ['C / R', 'Crop / Rotate'], ['M / B', 'Mark / Brush'],
              ['I', 'Pixel info'], ['Right-click', 'Context menu'],
              ['Space (hold) / Middle drag', 'Temporary pan'], ['Alt (hold)', 'Move layer / Invert brush'],
              ['F / 0', 'Fit image'], ['Shift+F', 'Fullscreen preview'], ['1', '100% zoom'], ['Q (hold)', 'Solo selection'],
              ['[ / ]', 'Brush / Info size − / +'], ['Enter', 'Apply crop'], ['Delete / Backspace', 'Delete selection'],
              ['Ctrl+D', 'Duplicate selection'], ['F2', 'Rename layer'], ['Ctrl+Z', 'Undo'],
              ['Ctrl+Shift+Z / Ctrl+Y', 'Redo'], ['Ctrl+S', 'Save checkpoint'],
              ['Ctrl / Shift+click', 'Select layers'], ['↑ / ↓ / Home / End', 'Navigate layer rows'],
              ['Enter / Space', 'Select focused layer'], ['Space (thumbnail)', 'Layer preview'],
              ['Enter / Esc (name)', 'Commit / Cancel rename'], ['Esc', 'Exit solo / Return to Pan'], ['?', 'Keyboard shortcuts'],
            ].map(([keys, action]) => <tr key={keys}><td><kbd>{keys}</kbd></td><td>{action}</td></tr>)}
          </tbody></table>
        </Popover>}
      </div>
      <div className="editor-title">
        <span id="project-size" className="mono">{meta.width} × {meta.height}</span></div>
      <div className="tool-group">
        <div className="export-split" aria-busy={exporting || fullResolution.busy}>
          <button className="action" id="export" title={solo ? 'Exports use the full layer stack, not the solo view' : 'Export image'}
            disabled={exporting || fullResolution.busy || !compositor?.ready} onClick={() => setImageExport(true)}>
            {exporting || fullResolution.busy ? <LoaderCircle className="export-spinner" /> : <Download />}
            {sheetRendering ? 'Rendering…' : 'Export'}</button>
          <button ref={exportAnchor} className="action export-toggle" aria-label="Export options"
            aria-haspopup="menu" aria-expanded={exportMenu} aria-controls={exportMenu ? 'export-menu' : undefined}
            disabled={exporting || fullResolution.busy || !compositor?.ready} onClick={() => setExportMenu(value => !value)}
            onKeyDown={event => {
              if (['ArrowDown', 'ArrowUp'].includes(event.key)) { event.preventDefault(); setExportMenu(true); }
            }}><ChevronDown /></button>
        </div>
        {exportMenu && <Popover anchor={exportAnchor} placement="bottom-end" onClose={() => setExportMenu(false)}>
          <div ref={exportMenuRef} id="export-menu" className="export-menu" role="menu" aria-label="Export"
            onKeyDown={event => {
              if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
              event.preventDefault();
              const items = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('[role=menuitem]')];
              const index = items.indexOf(document.activeElement as HTMLButtonElement);
              items[event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1
                : (index + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length]?.focus();
            }}>
            <button role="menuitem" onClick={() => chooseExport(false)}><ImageIcon />Image…</button>
            <button role="menuitem" onClick={() => chooseExport(true)}><LayoutPanelTop />Contact sheet</button>
          </div>
        </Popover>}
      </div>
    </div>
    <div className="editor-workspace">
      {compositor?.ready ? <LayerPanel meta={meta} flush={flush} compositor={compositor} /> : <aside className="layer-panel" />}
      <div className="image-workspace">
        <ToolOptions tool={tool} aspect={aspect} setAspect={setAspect} applyCrop={applyCrop} resetCrop={resetCrop}
          pending={!!pending} compositor={compositor} brush={brush} setBrush={setBrush} inverted={inverted}
          infoSize={infoSize} setInfoSize={setInfoSize} />
        <CanvasView meta={meta} handle={handle} tool={tool} setTool={setTool} aspect={aspect}
          pending={pending} onPending={setPending} onReady={setCompositor} onStatus={onStatus} zoom={status.zoom}
          brush={brush} inverted={inverted} spacePan={spacePan} infoSize={infoSize} onExport={() => setImageExport(true)}
          exportDisabled={exporting || fullResolution.busy || !compositor?.ready}
          onCheckpoint={() => {
            editorStore.getState().createCheckpoint();
            void flush().then(() => toast('Checkpoint saved')).catch((error: Error) => toast(error.message));
          }} />
        <SpectralRail meta={meta} />
      </div>
      <aside className="properties-panel">
        <ActiveLayer meta={meta} />
        <div className="property-tabs" role="tablist" aria-label="Layer properties">
          {(['color', 'geometry'] as const).map(tab => <button key={tab} data-properties={tab}
            className={propertyTab === tab ? 'active' : ''} onClick={() => setPropertyTab(tab)}>
            {tab[0].toUpperCase() + tab.slice(1)}</button>)}
        </div>
        <div id="properties" className="properties">{compositor?.ready && <Properties tab={propertyTab} meta={meta} resetCrop={resetCrop}
          onLayerCrop={() => setTool(tool === 'clip' ? 'pan' : 'clip')} cropping={tool === 'clip'}
          onCaption={() => setCaption(makeCaption(editorStore.getState().state!, meta))} />}</div>
      </aside>
    </div>
    <StatusBar><span id="editor-hint">{status.hint}</span>
      <span id="pixel-info" data-testid="pixel-info" className="mono">{status.pixel}</span></StatusBar>
    {caption !== null && <CaptionDialog caption={caption} onClose={() => setCaption(null)} />}
    {imageExport && <ImageExportDialog plan={fullResolution.plan} error={fullResolution.error} sessionId={sessionId}
      measurePreviewPNG={compositor?.previewScale === 1 ? measurePreviewPNG : undefined}
      preview={[Math.max(1,Math.round(frame.width)),Math.max(1,Math.round(frame.height))]}
      onClose={() => setImageExport(false)} onExport={options => {
        setImageExport(false);
        if (options.preview && compositor?.previewScale === 1) void exportPNG(options.save_as); else void fullResolution.start(options);
      }} />}
  </section>;
}
export function EditScreen() {
  const { data: session, error } = useSession(), { data: status } = useStatus();
  const [view, setView] = useState<{ session?: string; tool: Tool; tab: PropertyTab }>({ tool: 'pan', tab: 'color' });
  const setTool = useCallback((tool: Tool) => setView(old => ({
    session: session?.id, tool, tab: old.session === session?.id ? old.tab : 'color',
  })), [session?.id]);
  const setTab = useCallback((tab: PropertyTab) => setView(old => ({
    session: session?.id, tab, tool: old.session === session?.id ? old.tool : 'pan',
  })), [session?.id]);
  const loadedSession = useEditor(s => s.sessionId), loadedMeta = useEditor(s => s.meta);
  const meta = session?.meta as unknown as ProjectMeta | null;
  const projectKey = meta ? `${session?.id}:${meta.built_at}:${meta.layers.map(l => l.key).join(',')}` : null;
  const loadedKey = loadedMeta ? `${loadedSession}:${loadedMeta.built_at}:${loadedMeta.layers.map(l => l.key).join(',')}` : null;
  useEffect(() => {
    if (session && meta && projectKey !== loadedKey)
      editorStore.getState().load(session.id, meta, session.edits,
        status?.session === session.id ? status.edits?.revision : undefined);
  }, [session, meta, projectKey, loadedKey, status?.edits?.revision]);
  if (!session || !meta || projectKey !== loadedKey) return <section id="edit" className="screen active">
    <div className="empty-state"><h2>{error ? error.message : !meta ? 'Build images to start editing' : 'Loading composition…'}</h2></div>
    <StatusBar />
  </section>;
  return <LoadedEditor key={projectKey} sessionId={session.id} name={session.name} meta={meta}
    tool={view.session === session.id ? view.tool : 'pan'} setToolValue={setTool}
    propertyTab={view.session === session.id ? view.tab : 'color'} setPropertyTab={setTab} />;
}
