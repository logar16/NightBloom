import { useState } from 'react';
import { BookmarkCheck, Crosshair, LoaderCircle, Trash2 } from 'lucide-react';
import { useMutation } from '@tanstack/react-query';
import { api, queryClient } from '../../api/client';
import { type Bookmark, type BookmarkFrame, useBookmarks } from '../../api/bookmarks';
import { toast } from '../../api/hooks';
import { IconButton } from '../../ui';
import { waveStyle } from '../sky/utils';

function Note({ item }: { item: Bookmark }) {
  const [draft, setDraft] = useState(item.note);
  const save = useMutation({
    mutationFn: (note: string) => api<Bookmark>(`/bookmarks/${item.id}`, 'PATCH', { note }),
    onSuccess: updated => {
      queryClient.setQueryData<Bookmark[]>(['bookmarks'], items =>
        items?.map(value => value.id === updated.id ? updated : value));
    },
    onError: (error: Error) => toast(error.message),
  });
  return <input className="bookmark-note" aria-label={`Note for ${item.filter} ${item.target}`}
    title="Bookmark note · up to 200 characters" placeholder="Note" maxLength={200} value={draft}
    disabled={save.isPending} onChange={event => setDraft(event.target.value)}
    onBlur={() => { if (draft !== item.note) save.mutate(draft); }}
    onKeyDown={event => {
      if (event.key === 'Enter') event.currentTarget.blur();
      if (event.key === 'Escape') { setDraft(item.note); event.stopPropagation(); }
    }} />;
}

export function BookmarksPanel({ onStart }: { onStart: (frame: BookmarkFrame) => void }) {
  const bookmarks = useBookmarks();
  const [selected, setSelected] = useState(new Set<string>());
  const items = bookmarks.data ?? [];
  const selection = items.filter(item => selected.has(item.id));
  const groups = new Map<string, Bookmark[]>();
  for (const item of items) {
    const key = `${item.target || 'Untitled field'}\0${item.source_session}`;
    groups.set(key, [...(groups.get(key) ?? []), item]);
  }
  const start = useMutation({
    mutationFn: () => api<BookmarkFrame>('/bookmarks/frame', 'POST', { ids: selection.map(item => item.id) }),
    onSuccess: frame => {
      onStart({ ...frame, name: [...new Set(selection.map(item => item.target || item.source_name))].join(' · ').slice(0, 200) });
      toast('Bookmark frame ready');
    },
    onError: (error: Error) => toast(error.message),
  });
  return <article className="new-bookmarks" aria-labelledby="bookmarks-title">
    <div className="bookmark-heading">
      <div className="new-choice-heading"><BookmarkCheck /><h2 id="bookmarks-title">Bookmarks</h2>
        <span className="mono">{items.length}</span></div>
      <button className="action" disabled={!selection.length || start.isPending} onClick={() => start.mutate()}>
        {start.isPending ? <LoaderCircle className="export-spinner" /> : <Crosshair />}
        Start project here{selection.length > 0 && <span className="mono">{selection.length}</span>}
      </button>
    </div>
    {bookmarks.isPending && <span className="bookmark-empty" role="status">Loading bookmarks…</span>}
    {bookmarks.error && <div className="sky-callout" role="alert">{bookmarks.error.message}
      <button onClick={() => { void bookmarks.refetch(); }}>Retry</button></div>}
    {!bookmarks.isPending && !bookmarks.error && !items.length && <span className="bookmark-empty">No bookmarked images</span>}
    <div className="bookmark-groups">
      {[...groups].map(([key, group]) => <section key={key} className="bookmark-group">
        <header><h3>{group[0].target || 'Untitled field'}</h3><span>{group[0].source_name}</span></header>
        <div className="bookmark-items">{group.map(item => <div key={item.id}
          className={`bookmark-item ${selected.has(item.id) ? 'selected' : ''}`}>
          <label className="bookmark-pick">
            <input type="checkbox" checked={selected.has(item.id)}
              aria-label={`Select bookmark ${item.filter} ${item.target}`}
              onChange={() => setSelected(current => {
                const next = new Set(current); next.has(item.id) ? next.delete(item.id) : next.add(item.id); return next;
              })} />
            <img src={`/api/bookmarks/${item.id}/preview.jpg`} alt={item.filter} loading="lazy"
              onError={event => { event.currentTarget.style.visibility = 'hidden'; }} />
            <div className="bookmark-details">
              <div><span className="filter-chip" style={waveStyle(item.wavelength_nm)}>{item.filter}</span>
                <span>{item.collection} · {item.instrument}</span></div>
              <span className="mono" title="ICRS right ascension and declination">
                {item.center_icrs_deg.map(value => `${value.toFixed(5)}°`).join('  ')}
              </span>
            </div>
          </label>
          <IconButton icon={Trash2} title={`Remove bookmark ${item.filter}`} disabled={bookmarks.remove.isPending}
            onClick={() => bookmarks.remove.mutate(item.id)} />
          <Note item={item} />
        </div>)}</div>
      </section>)}
    </div>
  </article>;
}
