import { useRef, useState } from 'react';
import { HardDrive, Monitor, Moon, RotateCcw, Settings as SettingsIcon, Sun, X } from 'lucide-react';
import { ColorPicker, IconButton, Popover, Segmented } from '../ui';
import { resetAppearance, updateAppearance, useAppearance } from './store';
import type { Theme } from './appearance';
import './settings.css';
import { Storage } from './Storage';

export function Settings() {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [storage, setStorage] = useState(false);
  const appearance = useAppearance();
  return <>
    <button ref={anchor} id="settings" className={`icon ${open ? 'active' : ''}`}
      title="Settings" aria-label="Settings" aria-haspopup="dialog" aria-expanded={open}
      onClick={() => setOpen(!open)}><SettingsIcon /></button>
    {open && <Popover anchor={anchor} onClose={() => setOpen(false)} placement="bottom-end" className="settings-popover">
      <div role="dialog" aria-label="Settings">
        <div className="settings-heading"><h2>Appearance</h2>
          <IconButton autoFocus icon={X} title="Close settings" onClick={() => { setOpen(false); anchor.current?.focus(); }} />
        </div>
        <section className="settings-section"><h3>Theme</h3>
          <Segmented<Theme> label="Theme" value={appearance.theme}
            options={[
              { value: 'dark', label: 'Dark', icon: <Moon /> },
              { value: 'light', label: 'Light', icon: <Sun /> },
              { value: 'system', label: 'System', icon: <Monitor /> },
            ]} onChange={theme => updateAppearance({ theme })} />
        </section>
        <section className="settings-section"><h3>Accent colours</h3>
          <div className="settings-color"><span>Primary</span>
            <ColorPicker label="Primary accent" value={appearance.primary}
              presets={['#e9b35a', '#ef9273', '#9fb8f5', '#b7c97a', '#d6a1c4']}
              onInput={primary => updateAppearance({ primary })} />
          </div>
          <div className="settings-color"><span>Secondary</span>
            <ColorPicker label="Secondary accent" value={appearance.secondary}
              presets={['#5fc9d6', '#73bfa1', '#8faee4', '#c3b0e6', '#babfc6']}
              onInput={secondary => updateAppearance({ secondary })} />
          </div>
        </section>
        <button className="settings-reset subtle" onClick={resetAppearance}><RotateCcw />Reset to defaults</button>
        <button className="settings-storage" onClick={() => { setOpen(false); setStorage(true); }}><HardDrive />Storage</button>
      </div>
    </Popover>}
    {storage && <Storage onClose={() => { setStorage(false); anchor.current?.focus(); }} />}
  </>;
}
