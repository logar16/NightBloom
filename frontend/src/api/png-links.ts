export interface ExportLink { version: 1; project: string; checkpoint: string }

const signature = [137, 80, 78, 71, 13, 10, 26, 10];
const encoder = new TextEncoder();
const prefix = encoder.encode('nightbloom\0\0\0\0\0');
const decoder = new TextDecoder('utf-8', { fatal: true });

function crc32(bytes: Uint8Array) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (-(crc & 1) & 0xedb88320);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function validate(value: unknown): ExportLink {
  if (!value || typeof value !== 'object' || !('version' in value) || value.version !== 1)
    throw Error('Unsupported PNG saved-look metadata.');
  if (!('project' in value) || typeof value.project !== 'string' || !value.project
    || value.project.length > 255 || value.project.startsWith('.') || /[\\/:\u0000-\u001f]/.test(value.project)
    || !('checkpoint' in value) || typeof value.checkpoint !== 'string' || !/^[a-f0-9]{64}$/.test(value.checkpoint))
    throw Error('Invalid PNG saved-look link.');
  return { version: 1, project: value.project, checkpoint: value.checkpoint };
}

async function header(blob: Blob) {
  const bytes = new Uint8Array(await blob.slice(0, 33).arrayBuffer());
  if (bytes.length !== 33 || !signature.every((byte, index) => bytes[index] === byte)
    || new DataView(bytes.buffer).getUint32(8) !== 13 || decoder.decode(bytes.slice(12, 16)) !== 'IHDR')
    throw Error('Choose an exported PNG to open its saved look.');
}

export async function attachExportLink(png: Blob, value: ExportLink): Promise<Blob> {
  await header(png);
  const body = encoder.encode(JSON.stringify(validate(value)));
  const chunk = new Uint8Array(12 + prefix.length + body.length);
  const view = new DataView(chunk.buffer);
  view.setUint32(0, prefix.length + body.length);
  chunk.set(encoder.encode('iTXt'), 4);
  chunk.set(prefix, 8);
  chunk.set(body, 8 + prefix.length);
  view.setUint32(chunk.length - 4, crc32(chunk.subarray(4, -4)));
  return new Blob([png.slice(0, 33), chunk, png.slice(33)], { type: 'image/png' });
}

export async function readExportLink(png: Blob): Promise<ExportLink> {
  await header(png);
  let offset = 33;
  while (offset + 12 <= png.size) {
    const bytes = new Uint8Array(await png.slice(offset, offset + 8).arrayBuffer());
    const size = new DataView(bytes.buffer).getUint32(0);
    const kind = decoder.decode(bytes.slice(4));
    if (offset + size + 12 > png.size) throw Error('This PNG is incomplete.');
    if (kind === 'IDAT' || kind === 'IEND') break;
    if (kind === 'iTXt' && size >= 11) {
      const keyword = new Uint8Array(await png.slice(offset + 8, offset + 19).arrayBuffer());
      if (prefix.subarray(0, 11).every((byte, index) => keyword[index] === byte)) {
        if (size > 4096) throw Error('PNG saved-look metadata is too large.');
        const content = new Uint8Array(await png.slice(offset + 4, offset + size + 12).arrayBuffer());
        if (!prefix.every((byte, index) => content[index + 4] === byte))
          throw Error('Unsupported PNG saved-look metadata.');
        if (crc32(content.subarray(0, -4)) !== new DataView(content.buffer).getUint32(content.length - 4))
          throw Error('PNG saved-look metadata is damaged.');
        try { return validate(JSON.parse(decoder.decode(content.slice(4 + prefix.length, -4)))); }
        catch (error) {
          if (error instanceof SyntaxError || error instanceof TypeError) throw Error('PNG saved-look metadata is damaged.');
          throw error;
        }
      }
    }
    offset += size + 12;
  }
  throw Error('This PNG has no saved-look link. Open its project from the Gallery instead.');
}
