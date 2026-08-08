// docs/brand/pwa/favicon.ico wraps exactly one PNG-compressed image: a
// 6-byte ICONDIR header, one 16-byte ICONDIRENTRY, then the PNG verbatim.
// Rebuilding it from a recolored favicon-48.png is just re-stamping that
// fixed header around the new bytes — no ICO library needed, and none of
// the ambiguity a general-purpose multi-image encoder would introduce.

const HEADER_SIZE = 6;
const ENTRY_SIZE = 16;
const IMAGE_OFFSET = HEADER_SIZE + ENTRY_SIZE; // 22

/** Build a single-image, PNG-compressed .ico from a 48x48 PNG's bytes. */
export function buildFaviconIco(pngBytes, { width = 48, height = 48 } = {}) {
  const buf = Buffer.alloc(IMAGE_OFFSET + pngBytes.length);

  // ICONDIR: reserved=0, type=1 (icon), count=1
  buf.writeUInt16LE(0, 0);
  buf.writeUInt16LE(1, 2);
  buf.writeUInt16LE(1, 4);

  // ICONDIRENTRY
  buf.writeUInt8(width === 256 ? 0 : width, 6);
  buf.writeUInt8(height === 256 ? 0 : height, 7);
  buf.writeUInt8(0, 8); // color count (0 = not palette-indexed)
  buf.writeUInt8(0, 9); // reserved
  buf.writeUInt16LE(0, 10); // color planes
  buf.writeUInt16LE(32, 12); // bits per pixel
  buf.writeUInt32LE(pngBytes.length, 14); // size of PNG data
  buf.writeUInt32LE(IMAGE_OFFSET, 18); // offset to PNG data

  pngBytes.copy(buf, IMAGE_OFFSET);
  return buf;
}
