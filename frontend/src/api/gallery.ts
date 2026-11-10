import type { GalleryImage, GalleryProject } from './types';

function pixels(image: GalleryImage) {
  return (image.width ?? 0) * (image.height ?? 0);
}

function newest(a: GalleryImage, b: GalleryImage) {
  return b.modified - a.modified || a.filename.localeCompare(b.filename);
}

function longEdge(image: GalleryImage) {
  return Math.max(image.width ?? 0, image.height ?? 0);
}

export function galleryCover(images: readonly GalleryImage[]) {
  const healthy = images.filter(image => !image.error);
  const photos = healthy.filter(image => !image.filename.split('/').at(-1)!.startsWith('contact-sheet-'));
  const candidates = photos.length ? photos : healthy.length ? healthy : images;
  const largest = candidates.reduce((side, image) => Math.max(side, longEdge(image)), 0);
  return candidates.filter(image => longEdge(image) >= largest * .9).sort(newest)[0];
}

export function sortExports(images: readonly GalleryImage[]) {
  const timed = [...images].sort(newest);
  const ordered: GalleryImage[] = [];
  for (let start = 0; start < timed.length;) {
    let end = start + 1;
    // Anchor batches to their newest export, rather than chaining nearby pairs.
    while (end < timed.length && timed[start].modified - timed[end].modified <= 5 * 60) end++;
    ordered.push(...timed.slice(start, end).sort((a, b) => pixels(b) - pixels(a) || newest(a, b)));
    start = end;
  }
  return ordered;
}

export function orderGallery(projects: GalleryProject[]) {
  return projects.map(project => ({ ...project, images: sortExports(project.images) }));
}
