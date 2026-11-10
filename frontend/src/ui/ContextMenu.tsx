import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from 'react';
import { createPortal } from 'react-dom';
import { ChevronLeft, ChevronRight, type LucideIcon } from 'lucide-react';
import './context-menu.css';

export interface MenuItem {
  label: string;
  icon?: LucideIcon;
  shortcut?: string;
  disabled?: boolean;
  danger?: boolean;
  action?: () => void;
  children?: MenuEntry[];
}
export type MenuEntry = MenuItem | 'separator';
export interface MenuPosition { x: number; y: number }

export function floatingPosition(x: number, y: number, width: number, height: number, gap = 0): CSSProperties {
  return {
    left: Math.max(8, Math.min(x + gap + width > innerWidth - 8 ? x - width - gap : x + gap, innerWidth - width - 8)),
    top: Math.max(8, Math.min(y + gap + height > innerHeight - 8 ? y - height - gap : y + gap, innerHeight - height - 8)),
  };
}

export function ContextMenu({ position, items, label, onClose }: {
  position: MenuPosition; items: MenuEntry[]; label: string; onClose: () => void;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const [path, setPath] = useState<MenuItem[]>([]);
  const [style, setStyle] = useState<CSSProperties>({ visibility: 'hidden' });
  const close = useRef(onClose); close.current = onClose;
  const restore = useRef<HTMLElement | null>(document.activeElement as HTMLElement);
  const entries = path.at(-1)?.children ?? items;
  useLayoutEffect(() => {
    const place = () => {
      if (!panel.current) return;
      const bounds = panel.current.getBoundingClientRect();
      setStyle(floatingPosition(position.x, position.y, bounds.width, bounds.height));
    };
    const observer = new ResizeObserver(place);
    if (panel.current) observer.observe(panel.current);
    place();
    panel.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus({ preventScroll: true });
    return () => observer.disconnect();
  }, [position, path]);
  useEffect(() => {
    const outside = (event: Event) => {
      if (!panel.current?.contains(event.target as Node)) close.current();
    };
    const scroll = (event: Event) => {
      if (!(event.target instanceof Node) || !panel.current?.contains(event.target)) close.current();
    };
    const dismiss = () => close.current();
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('focusin', outside);
    window.addEventListener('scroll', scroll, true);
    window.addEventListener('resize', dismiss);
    window.addEventListener('blur', dismiss);
    return () => {
      document.removeEventListener('pointerdown', outside, true);
      document.removeEventListener('focusin', outside);
      window.removeEventListener('scroll', scroll, true);
      window.removeEventListener('resize', dismiss);
      window.removeEventListener('blur', dismiss);
    };
  }, []);
  function finish(item: MenuItem) {
    if (item.children) { setPath([...path, item]); return; }
    close.current();
    restore.current?.focus({ preventScroll: true });
    item.action?.();
  }
  return createPortal(<div ref={panel} className="ui-popover ui-context-menu" style={style}
    role="menu" aria-label={path.at(-1)?.label ?? label} onContextMenu={event => event.preventDefault()}
    onKeyDown={event => {
      event.stopPropagation();
      const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
      const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
      if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
        event.preventDefault();
        buttons[event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1
          : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length]?.focus();
      } else if (event.key === 'Escape') {
        event.preventDefault(); close.current(); restore.current?.focus({ preventScroll: true });
      } else if (event.key === 'ArrowLeft' && path.length) {
        event.preventDefault(); setPath(path.slice(0, -1));
      } else if (event.key === 'ArrowRight' && document.activeElement?.getAttribute('aria-haspopup') === 'menu') {
        event.preventDefault(); (document.activeElement as HTMLButtonElement).click();
      }
    }}>
    {path.length > 0 && <><button role="menuitem" className="context-back" onClick={() => setPath(path.slice(0, -1))}>
      <ChevronLeft /><span>{path.at(-1)!.label}</span></button><div role="separator" /></>}
    {entries.map((item, index) => item === 'separator' ? <div key={index} role="separator" />
      : <button key={`${index}-${item.label}`} role="menuitem" disabled={item.disabled}
        className={item.danger ? 'context-danger' : undefined}
        aria-haspopup={item.children ? 'menu' : undefined} onClick={() => finish(item)}>
        {item.icon ? <item.icon /> : <span className="context-icon" />}
        <span className="context-label">{item.label}</span>
        {item.shortcut && <kbd>{item.shortcut}</kbd>}{item.children && <ChevronRight />}
      </button>)}
  </div>, document.body);
}
