import { useEffect, useRef, useState } from 'react';
import { api } from './api/client';
import { refreshSession, toast } from './api/hooks';

export function ProjectName({ name, shortcut = false }: { name: string; shortcut?: boolean }) {
  const [editing, setEditing] = useState(false), [value, setValue] = useState(name);
  const cancelled = useRef(false);
  useEffect(() => {
    if (!shortcut) return;
    const rename = (event: KeyboardEvent) => {
      if (event.key !== 'F2' || event.defaultPrevented || document.querySelector('dialog[open]')
        || (event.target instanceof Element && event.target.closest('input,textarea,select,[contenteditable=true],.ui-popover'))) return;
      event.preventDefault(); cancelled.current = false; setValue(name); setEditing(true);
    };
    document.addEventListener('keydown', rename);
    return () => document.removeEventListener('keydown', rename);
  }, [name, shortcut]);
  const save = () => {
    if (!editing || cancelled.current) return;
    setEditing(false);
    const next = value.trim();
    if (!next || next === name) return;
    void api('/session', 'PATCH', { name: next }).then(refreshSession).catch((error: Error) => toast(error.message));
  };
  return editing
    ? <input id="project-name" className="project-name-input" autoFocus aria-label="Project name" value={value}
      maxLength={200} onChange={event => setValue(event.target.value)} onBlur={save} onFocus={event => event.target.select()}
      onKeyDown={event => {
        event.stopPropagation();
        if (event.key === 'Enter') save();
        if (event.key === 'Escape') { cancelled.current = true; setEditing(false); }
      }} />
    : <button id="project-name" className="project-name" title={shortcut ? 'Rename project (F2)' : 'Rename project'}
      onClick={() => { cancelled.current = false; setValue(name); setEditing(true); }}>{name || 'Composition'}</button>;
}
