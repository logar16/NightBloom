import { expect, it } from 'vitest';
import type { Observation, Session } from '../../api/types';
import { previewUrl } from './utils';

const observation: Observation = {
  obs_id: 'survey-CDS-P-DSS2-red', obsid: '', collection: 'Survey', instrument: 'DSS2',
  filter: 'red', target: '', title: '', wavelength_nm: 650, preview_uri: null, pixels: [], info: {},
};
const view: Session['view'] = {
  center_icrs_deg: [10, 20], field_arcmin: [30, 20], pixel_dimensions: [900, 600],
  captured_at_utc: '2026-11-05T18:00:00Z', rotation_deg: 0,
};

it.each(['thumb', 'large'] as const)('keys cached survey %s imagery by its captured sky frame', size => {
  const original = previewUrl(observation, size, view);
  expect(previewUrl(observation, size, { ...view })).toBe(original);
  expect(previewUrl(observation, size, {
    ...view, captured_at_utc: '2026-11-05T18:01:00Z', center_icrs_deg: [15, 25],
  })).not.toBe(original);
  expect(previewUrl(observation, size, { ...view, rotation_deg: 90 })).not.toBe(original);
  expect(previewUrl(observation, size, { ...view, pixel_dimensions: [1800, 1200] })).not.toBe(original);
  expect(previewUrl(observation, size, view, 1)).not.toBe(original);

  const legacy = { ...view, captured_at_utc: undefined };
  expect(previewUrl(observation, size, { ...legacy, center_icrs_deg: [15, 25] }))
    .not.toBe(previewUrl(observation, size, legacy));
});

it('retains the cache of whole archive images when only the captured view changes', () => {
  const archive = { ...observation, obs_id: 'hst-example', collection: 'HST' };
  const original = previewUrl(archive, 'large', view);
  expect(previewUrl(archive, 'large', { ...view, center_icrs_deg: [15, 25] })).toBe(original);
  expect(previewUrl(archive, 'large', view, 1)).not.toBe(original);
  expect(previewUrl(archive, 'thumb', view)).not.toBe(original);
});
