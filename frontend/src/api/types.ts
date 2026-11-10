export interface Observation {
  obs_id: string;
  obsid: string;
  collection: string;
  instrument: string;
  filter: string;
  target: string;
  title: string;
  wavelength_nm: number | null;
  fwhm_arcsec?: number;
  soft?: boolean;
  soft_reason?: string;
  preview_uri: string | null;
  pixels: number[][][];
  info: { line?: string; shows?: string; kind?: string; band?: string };
}
export interface Group {
  id: string;
  collection: string;
  instrument: string;
  targets: string[];
  title: string;
  field?: string;
  proposal?: string;
  date: string;
  filters: string[];
  observations: Observation[];
}
export interface Session {
  build?: { mosaic: boolean };
  id: string;
  name: string;
  view: {
    source?: string;
    center_icrs_deg: number[];
    field_arcmin: number[];
    pixel_dimensions: number[];
    captured_at_utc?: string;
    rotation_deg?: number;
  };
  catalog: { groups: Group[]; observations_in_view?: number; mast_searched?: boolean; survey_warning?: string };
  selection: { obs_ids: string[] };
  triage: { favorites: string[]; dismissed: string[] };
  meta: Record<string, unknown> | null;
  edits: Record<string, unknown> | null;
  build_version?: string | number;
  rebuild_required?: boolean;
}
export interface CoordinateCapture {
  ra: number; dec: number; name: string; width_arcmin: number;
  aspect: string; rotation_deg: number; surveys_only?: boolean;
}
export interface SessionSummary {
  id: string; name: string; created: string; updated: string;
  observations: number; selected: number; groups: number; built: boolean;
}
export interface GalleryImage {
  filename: string; width: number | null; height: number | null; size_bytes: number;
  modified: number; version: string; error: string | null;
  checkpoint?: string | null;
  checkpoint_error?: string | null;
}
export interface GalleryProject {
  id: string; name: string; updated: number; images: GalleryImage[];
}
export interface PlanFile {
  obs_id: string; filename: string; filter: string; size_bytes: number | null;
  cached?: boolean; done_bytes?: number;
  transfer_bytes?: number | null; transferred_bytes?: number;
  transfer_estimated?: boolean; transfer_note?: string;
  source_fields?: { name: string; size_bytes: number; cached: boolean }[];
  progress?: number;
  state?: string; error?: string;
  outside_fraction?: number | null;
}
export interface BuildGrid {
  width: number; height: number; pixel_scale_arcsec: number; wcs: Record<string, unknown>;
}
export interface CanvasRect { x: number; y: number; w: number; h: number }
export interface CanvasPreview extends BuildGrid {
  id: string; image_url: string;
  current_scale_arcsec: number | null;
  current_rect: CanvasRect | null;
  default_region: CanvasRect;
  footprints: { key: string; filter: string; color: string; points: [number, number][]; native_scale_arcsec: number }[];
}
export interface CanvasPlan {
  grid: BuildGrid;
  kept: { key: string; kept_fraction: number }[];
  detail_ratio: number;
}
export interface BuildPlan {
  layer_count?: number;
  files: PlanFile[]; total_bytes: number; cached_bytes: number;
  grid_locked?: boolean; grid?: BuildGrid | null;
}
export interface Job {
  session: string; name: string;
  waiting_for?: { session: string; name: string; stage: string; progress: number } | null;
  id: string; kind: string; state: string; stage: string; phase?: string;
  progress: number; message: string; log: string[]; error?: string;
  files?: PlanFile[];
  steps?: { key: string; label: string; state: string; progress: number; eta_s?: number | null }[];
  eta_s?: number | null;
  speed_bytes_s?: number;
  result?: { path: string; download_url?: string; width?: number; height?: number } | null;
  cancel_requested?: boolean;
}
export interface Status {
  jobs?: Job[];
  aladin: boolean; stale?: boolean; restartable?: boolean; session: string | null; job: Job | null;
  services?: Record<'cds' | 'mast', { ok: boolean | null; checked: string | null }>;
  edits: { revision: number; source?: string; note?: string } | null;
}
