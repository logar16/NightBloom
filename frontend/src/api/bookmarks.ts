import { useMutation, useQuery } from '@tanstack/react-query';
import { api, queryClient } from './client';
import { toast } from './hooks';

export interface Bookmark {
  id: string; obs_id: string; collection: string; instrument: string; filter: string;
  target: string; wavelength_nm: number | null; region: string;
  center_icrs_deg: [number, number]; size_arcmin: [number, number];
  preview: string | null; source_session: string; source_name: string; note: string; created: string;
}
export interface BookmarkFrame {
  ra: number; dec: number; aspect: string; width_arcmin: number; height_arcmin: number;
  required_width_arcmin: number; exceeds_limit: boolean; warning: string | null; name: string;
}
const key = ['bookmarks'];
export function useBookmarks() {
  const query = useQuery({
    queryKey: key, queryFn: () => api<Bookmark[]>('/bookmarks'),
    refetchOnWindowFocus: true,
  });
  const toggle = useMutation({
    scope: { id: 'bookmarks' },
    mutationFn: async ({ ids, session }: { ids: string[]; session: string }) => {
      const current = queryClient.getQueryData<Bookmark[]>(key) ?? [];
      const matches = current.filter(item => ids.includes(item.obs_id));
      const remove = ids.every(id => matches.some(item => item.obs_id === id));
      if (remove) {
        await Promise.all(matches.map(item => api(`/bookmarks/${item.id}`, 'DELETE')));
        return { items: current.filter(item => !ids.includes(item.obs_id)), remove };
      }
      return { items: await api<Bookmark[]>('/bookmarks', 'POST', { session, obs_ids: ids }), remove };
    },
    onSuccess: ({ items, remove }) => {
      queryClient.setQueryData(key, items);
      toast(remove ? 'Bookmarks removed' : 'Images bookmarked');
    },
    onError: (error: Error) => toast(error.message),
    onSettled: () => { void queryClient.invalidateQueries({ queryKey: key }); },
  });
  const remove = useMutation({
    scope: { id: 'bookmarks' },
    mutationFn: (id: string) => api(`/bookmarks/${id}`, 'DELETE'),
    onSuccess: (_, id) => {
      queryClient.setQueryData<Bookmark[]>(key, items => items?.filter(item => item.id !== id));
      toast('Bookmark removed');
    },
    onError: (error: Error) => toast(error.message),
    onSettled: () => { void queryClient.invalidateQueries({ queryKey: key }); },
  });
  return { ...query, toggle, remove };
}
