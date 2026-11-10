import { useEffect, useRef, useState } from 'react';
import { Check, Database, FolderOpen, HardDrive, LockKeyhole, RefreshCw, Trash2, TriangleAlert, X } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { api } from '../api/client';
import { isDesktop } from '../api/desktop';
import { toast, useStatus } from '../api/hooks';
import { IconButton } from '../ui';
import './storage.css';

type Reference = { id: string; name: string };
type Download = {
  path: string; name: string; size: number; status: 'in_use' | 'trash_only' | 'orphan';
  instrument?: string; filter?: string;
  references: Record<'live' | 'trash' | 'backups' | 'fixtures', Reference[]>;
};
type Folder = { id: string; name: string; size: number; date: string };
type Category = 'downloads' | 'trash' | 'backups' | 'caches';
type Item = { id: string; name: string; size: number; detail?: string };
type Inventory = {
  total: number; totals: Record<string, number>;
  downloads: Download[]; trash: Folder[]; backups: Folder[];
  can_cleanup?: boolean; missing_sources?: number;
};
export function bytes(value: number) {
  if (!value) return '0 B';
  const index = Math.min(3, Math.floor(Math.log(value) / Math.log(1000)));
  return `${(value / 1000 ** index).toLocaleString(undefined, { maximumFractionDigits: index ? 1 : 0 })} ${['B', 'KB', 'MB', 'GB'][index]}`;
}
const categories = [
  ['downloads', 'Downloads'], ['aligned', 'Aligned cache'], ['previews', 'Thumbnails'],
  ['sessions', 'Projects'], ['trash', 'Trash'], ['backups', 'Backups'], ['fixtures', 'Test fixtures'],
  ['exports', 'Exports'], ['runtime', 'Browser data'],
] as const;

type LibraryLocation = { root: string; pending_root: string | null; can_change: boolean; available_bytes: number };
function LibraryFolder() {
  const query = useQuery({ queryKey: ['desktop-storage'], queryFn: () => api<LibraryLocation>('/desktop/storage') });
  const [path, setPath] = useState(''), [saving, setSaving] = useState(false), [error, setError] = useState('');
  const location = query.data;
  useEffect(() => { if (location) setPath(location.pending_root || location.root); }, [location]);
  async function pick() {
    setSaving(true); setError('');
    try {
      const result = await api<{ path: string | null }>('/desktop/storage/pick', 'POST');
      if (result.path) setPath(result.path);
    } catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { setSaving(false); }
  }
  async function apply() {
    setSaving(true); setError('');
    try {
      await api<LibraryLocation>('/desktop/storage', 'POST', { path });
      await query.refetch();
    } catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { setSaving(false); }
  }
  return <section className="storage-library">
    <div className="storage-library-heading"><label htmlFor="library-folder">Library folder</label>
      {location && <span className="mono">{bytes(location.available_bytes)} free</span>}</div>
    <div className="storage-library-path">
      <input id="library-folder" value={path} onChange={event => setPath(event.target.value)}
        disabled={saving || !location?.can_change} title={location?.can_change ? location.root : 'This launch uses --data'} />
      <IconButton icon={FolderOpen} title="Choose library folder" disabled={saving || !location?.can_change} onClick={() => { void pick(); }} />
      <IconButton icon={Check} title="Apply library folder"
        disabled={saving || !location?.can_change || !path || path === (location.pending_root || location.root)}
        onClick={() => { void apply(); }} />
    </div>
    {location?.pending_root && <p role="status">Transfer on next launch · {location.pending_root}</p>}
    {(error || query.error) && <p role="alert">{error || query.error?.message}</p>}
  </section>;
}

function SelectAll({ checked, partial, label, onChange, disabled }: {
  checked: boolean; partial: boolean; label: string; onChange: () => void; disabled: boolean;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => { if (ref.current) ref.current.indeterminate = partial; }, [partial]);
  return <input ref={ref} type="checkbox" checked={checked} aria-label={label}
    disabled={disabled} onChange={onChange} />;
}

export function Storage({ onClose }: { onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const query = useQuery({ queryKey: ['storage'], queryFn: () => api<Inventory>('/storage'), staleTime: 0 });
  const busy = useStatus().data?.jobs?.some(job => ['running', 'queued'].includes(job.state));
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirm, setConfirm] = useState(false), [deleting, setDeleting] = useState(false);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const data = query.data;
  const canCleanup = data?.can_cleanup !== false;
  const shownCategories = categories.filter(([id]) => (data?.totals[id] ?? 0) > 0);
  useEffect(() => { dialog.current?.showModal(); }, []);
  const key = (category: Category, id: string) => `${category}:${id}`;
  const toggle = (category: Category, items: Item[], remove: boolean) => {
    setConfirm(false); setMessage('');
    setSelected(previous => {
      const next = new Set(previous);
      items.forEach(item => remove ? next.delete(key(category, item.id)) : next.add(key(category, item.id)));
      return next;
    });
  };
  const downloads = (status: Download['status']): Item[] => (data?.downloads ?? [])
    .filter(item => item.status === status).map(item => ({
      id: item.path, name: item.name, size: item.size,
      detail: [
        [item.instrument, item.filter].filter(Boolean).join(' / '),
        ...Object.entries(item.references).flatMap(([category, refs]) => refs.map(ref =>
          `${ref.name}${category === 'trash' ? ' (deleted)' : category === 'backups' ? ' (backup)' : category === 'fixtures' ? ' (test fixture)' : ''}`)),
      ].filter(Boolean).join(' · ') || 'No project references',
    }));
  const folders = (category: 'trash' | 'backups'): Item[] => (data?.[category] ?? []).map(item => ({
    ...item, detail: `${category === 'trash' ? 'Deleted' : 'Saved'} ${new Date(item.date).toLocaleDateString()} · ${item.id}`,
  }));
  const cacheItems: Item[] = [
    { id: 'aligned', name: 'Aligned cache', size: data?.totals.aligned ?? 0,
      detail: 'Rebuilt automatically · next build will be slower' },
    { id: 'previews', name: 'Preview thumbnails', size: data?.totals.previews ?? 0,
      detail: 'Downloaded again when needed' },
  ].filter(item => item.size > 0);
  const selectable: [Category, Item[]][] = [
    ['downloads', [...downloads('trash_only'), ...downloads('orphan')]],
    ['trash', folders('trash')], ['backups', folders('backups')], ['caches', cacheItems],
  ];
  const chosen = selectable.flatMap(([category, items]) => items.filter(item => selected.has(key(category, item.id))));
  const total = chosen.reduce((sum, item) => sum + item.size, 0);
  const disabled = deleting || !!busy;
  const group = (category: Category, title: string, items: Item[], note?: string, status?: string) => {
    const count = items.filter(item => selected.has(key(category, item.id))).length;
    return <section className="storage-group" key={title}>
      <div className="storage-group-heading">
        <SelectAll checked={!!items.length && count === items.length} partial={count > 0 && count < items.length}
          label={`Select all ${title.toLowerCase()}`} disabled={disabled || !items.length}
          onChange={() => toggle(category, items, count === items.length)} />
        <h3 className={status ? `storage-badge ${status}` : ''}>{title}</h3>
        <span>{items.length} {category === 'downloads' ? (items.length === 1 ? 'file' : 'files') : (items.length === 1 ? 'item' : 'items')} · {bytes(items.reduce((sum, item) => sum + item.size, 0))}</span>
      </div>
      {note && <p className="storage-note">{note}</p>}
      {!items.length && <p className="storage-empty">Nothing to clean up</p>}
      {items.map(item => <label className="storage-row" key={item.id}>
        <input type="checkbox" checked={selected.has(key(category, item.id))} disabled={disabled}
          onChange={() => toggle(category, [item], selected.has(key(category, item.id)))} />
        <span className="storage-file"><span title={item.id}>{item.name}</span><small>{item.detail}</small></span>
        <span className="storage-size">{bytes(item.size)}</span>
      </label>)}
    </section>;
  };
  const remove = async () => {
    if (!confirm) { setConfirm(true); return; }
    setDeleting(true); setError('');
    const body = Object.fromEntries(selectable.map(([category, items]) => [
      category, items.filter(item => selected.has(key(category, item.id))).map(item => item.id),
    ]));
    try {
      const result = await api<{ freed_bytes: number }>('/storage/delete', 'POST', body);
      setSelected(new Set()); setConfirm(false);
      toast(`Freed ${bytes(result.freed_bytes)}`);
      setMessage(`Freed ${bytes(result.freed_bytes)}`);
      await query.refetch();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : 'Cleanup failed. Refresh and try again.');
      setConfirm(false);
      await query.refetch();
    } finally { setDeleting(false); }
  };
  return <dialog ref={dialog} className="storage-dialog" aria-labelledby="storage-title"
    onClose={onClose} onCancel={event => { if (deleting) event.preventDefault(); }}>
    <header className="dialog-bar">
      <h2 id="storage-title"><HardDrive />Storage</h2>
      <div className="storage-header-actions"><IconButton icon={RefreshCw} title="Refresh storage"
        disabled={deleting || query.isFetching} onClick={() => { setConfirm(false); void query.refetch(); }} />
        <IconButton icon={X} title="Close storage" disabled={deleting} onClick={() => dialog.current?.close()} /></div>
    </header>
    <div className="storage-scroll">
      {isDesktop() && <LibraryFolder />}
      {query.isPending && <p role="status">Measuring storage…</p>}
      {query.error && <p role="alert">{query.error.message} Try refreshing.</p>}
      {data && <>
        <div className="storage-summary"><span>On this device</span><strong>{bytes(data.total)}</strong></div>
        <div className="storage-bar" role="img" aria-label={shownCategories.map(([id, label]) => `${label}: ${bytes(data.totals[id] ?? 0)}`).join(', ')}>
          {shownCategories.map(([id, label]) => <span key={id} className={`storage-category-${id}`}
            style={{ flexGrow: data.totals[id] ?? 0 }} title={`${label} · ${bytes(data.totals[id] ?? 0)}`} />)}
        </div>
        <div className="storage-legend">{shownCategories.map(([id, label]) => <span key={id}>
          <i className={`storage-category-${id}`} />{label}<b>{bytes(data.totals[id] ?? 0)}</b></span>)}</div>
        {!!data.missing_sources && <p className="storage-warning" role="status"><TriangleAlert />
          {data.missing_sources} referenced FITS files unavailable</p>}
        {canCleanup && <><h2 className="storage-section-title"><Database />Downloads</h2>
        {group('downloads', 'Only in deleted projects', downloads('trash_only'),
          'Referenced only by deleted projects or backups.', 'trash-only')}
        {group('downloads', 'Unused', downloads('orphan'), undefined, 'unused')}
        <details className="storage-protected">
          <summary><LockKeyhole />In use <span>{downloads('in_use').length} {downloads('in_use').length === 1 ? 'file' : 'files'} · {bytes(downloads('in_use').reduce((sum, item) => sum + item.size, 0))}</span></summary>
          <p className="storage-note">Protected by live projects or test fixtures.</p>
          {downloads('in_use').map(item => <div className="storage-row" key={item.id}>
            <LockKeyhole /><span className="storage-file"><span title={item.id}>{item.name}</span><small>{item.detail}</small></span>
            <span className="storage-size">{bytes(item.size)}</span>
          </div>)}
        </details>
        {group('trash', 'Deleted projects (trash)', folders('trash'),
          'Permanent deletion. Their downloads become Unused unless another project still needs them.')}
        {!!data.backups.length && group('backups', 'Backups', folders('backups'), 'Recovery copies · permanent deletion.')}
        {group('caches', 'Caches', cacheItems)}</>}
      </>}
    </div>
    {(canCleanup || error || message || busy) && <footer className="storage-footer">
      <div aria-live="polite">{error ? <span role="alert" className="storage-error">{error}</span>
        : message ? <span role="status">{message}</span>
        : busy ? <span>A job is running. Cleanup is paused.</span>
        : confirm ? <span className="storage-error">Delete permanently? This cannot be undone.</span>
        : canCleanup ? <span>Live projects and test fixtures are protected.</span> : null}</div>
      {canCleanup && <div className="storage-footer-actions">
        {confirm && <button disabled={deleting} onClick={() => setConfirm(false)}>Cancel</button>}
        <button className="storage-delete" disabled={disabled || !chosen.length || query.isFetching}
          onClick={() => { void remove(); }}><Trash2 />{deleting ? 'Deleting…' : confirm ? 'Delete permanently' : `Delete selected (${chosen.length} items · ${bytes(total)})`}</button>
      </div>}
    </footer>}
  </dialog>;
}
