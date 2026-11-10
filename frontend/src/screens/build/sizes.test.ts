import { describe, expect, it } from 'vitest';
import type { Job, PlanFile } from '../../api/types';
import { jobProgress, sourceFieldsLabel, transferLabel, transferTotal, transferredBytes } from './sizes';

const file = (values: Partial<PlanFile>): PlanFile => ({
  obs_id: 'survey', filename: 'survey.fits', filter: 'W3', size_bytes: null, ...values,
});

describe('Build transfer sizes', () => {
  it('uses upstream estimates rather than final FITS size, excluding cached files', () => {
    const files = [file({ size_bytes: 4_000_000, transfer_bytes: 180_000_000, transfer_estimated: true }),
      file({ cached: true, size_bytes: 80_000_000, done_bytes: 80_000_000 })];
    expect(transferLabel(files)).toBe('~180.0 MB');
    expect(transferTotal(files)).toBe(180_000_000);
    expect(transferredBytes(files[1])).toBe(0);
  });
  it('keeps fully and partially unknown transfers explicit', () => {
    expect(transferLabel([file({})])).toBe('Size pending');
    expect(transferLabel([file({ transfer_bytes: 0 }), file({})])).toBe('Size pending');
    const mixed = [file({ transfer_bytes: 50_000_000, transfer_estimated: true }), file({})];
    expect(transferLabel(mixed)).toBe('~50.0 MB + unknown');
    expect(transferTotal(mixed)).toBeNull();
  });
  it('shows completed actual payloads without an approximation marker', () => {
    const actual = file({ size_bytes: 4_000_000, transfer_bytes: 165_000_000,
      transferred_bytes: 165_000_000, done_bytes: 4_000_000, transfer_estimated: false });
    expect(transferLabel([actual])).toBe('165.0 MB');
    expect(transferredBytes(actual)).toBe(165_000_000);
  });
  it('shares byte progress with Jobs, without counting cached storage or declaring early completion', () => {
    const job: Job = { id: 'build', session: 'one', name: 'One', state: 'running', message: '', log: [],
      kind: 'build', stage: 'download', progress: 0, files: [
      file({ transfer_bytes: 100, transferred_bytes: 90, state: 'downloading' }),
      file({ size_bytes: 1000, cached: true, state: 'cached' }),
    ] };
    expect(jobProgress(job)).toBe(.9);
    job.files![0].transferred_bytes = 110;
    expect(jobProgress(job)).toBe(.99);
    job.files![0].transfer_bytes = null;
    job.progress = .5;
    expect(jobProgress(job)).toBe(.5);
    expect(jobProgress({ ...job, stage: 'align', progress: .2 })).toBe(.2);
  });
  it('separates cached MDW field sizes from remaining transfer', () => {
    const mdw = file({ transfer_bytes: 64_000_000, transfer_estimated: true, source_fields: [
      { name: 'Field 1', size_bytes: 64_000_000, cached: false },
      { name: 'Field 2', size_bytes: 64_000_000, cached: true },
    ] });
    expect(sourceFieldsLabel(mdw)).toBe('2 fields · ~64.0 MB/field · 1 to fetch · 1 cached');
    expect(transferLabel([mdw])).toBe('~64.0 MB');
    mdw.source_fields![0].cached = true;
    mdw.transfer_bytes = 0;
    expect(sourceFieldsLabel(mdw)).toBe('2 fields · ~64.0 MB/field · 2 cached');
    expect(transferLabel([mdw])).toBe('0 B');
  });
});
