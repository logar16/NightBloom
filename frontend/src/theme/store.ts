import { useEffect } from 'react';
import { create } from 'zustand';
import { applyAppearance, defaults, parseAppearance, readAppearance, saveAppearance, type Appearance } from './appearance';

export const useAppearance = create<Appearance>(() => readAppearance());

export function updateAppearance(change: Partial<Appearance>) {
  const value = parseAppearance({ ...useAppearance.getState(), ...change });
  applyAppearance(value);
  saveAppearance(value);
  useAppearance.setState(value);
}
export function resetAppearance() { updateAppearance(defaults); }

export function useSystemTheme() {
  useEffect(() => {
    const query = matchMedia('(prefers-color-scheme: light)');
    const update = () => applyAppearance(useAppearance.getState());
    query.addEventListener('change', update);
    update();
    return () => query.removeEventListener('change', update);
  }, []);
}
