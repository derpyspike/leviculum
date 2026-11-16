//! The GeoTIFF subset the ground inputs ship, read through byte ranges:
//! little-endian classic TIFF, one band of `u8`, `u16` or `f32`, in
//! strips or tiles, uncompressed, LZW (Kellndorfer's tiles) or DEFLATE
//! with or without the horizontal predictor (GeoMAD's COGs), with the
//! overviews of a cloud-optimised file as further images. Only the
//! chunks a window needs are fetched, so a GeoMAD overview of a 96 km
//! tile costs its own bytes, not the 160 MB of the full image. Anything
//! else is refused by name.
//!
//! The height cut keeps its own stricter reader for GLO-30
//! (`crate::geotiff`); both read the same tag layout.

use std::io::Read;

use leviculum_mapcut::pmtiles::RangeRead;
use leviculum_mapcut::Res;

fn err(what: &str) -> Box<dyn std::error::Error + Send + Sync> {
    format!("ground input: unsupported or broken TIFF: {what}").into()
}

/// The largest image edge read, in pixels (GeoMAD's full image is 9600).
const MAX_EDGE: u64 = 20_000;
/// The largest chunk read, in bytes.
const MAX_CHUNK: u64 = 64 << 20;
/// The header bytes fetched first: the IFDs of every input fit.
const PREFIX: u64 = 1 << 16;

/// One image of the file.
#[derive(Clone, Debug)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    bits: u16,
    float: bool,
    compression: u16,
    predictor: u16,
    /// Chunk edge: tile (w, h), or (width, rows per strip).
    chunk: (usize, usize),
    offsets: Vec<u64>,
    counts: Vec<u64>,
    /// ModelPixelScale and ModelTiepoint of the full image, if given.
    pub scale: Option<[f64; 2]>,
    pub tie: Option<[f64; 4]>,
    pub nodata: Option<f64>,
}

impl Image {
    /// The compressed bytes of every chunk of the image.
    pub fn chunk_bytes(&self) -> u64 {
        self.counts.iter().sum()
    }
}

/// A file's images, the full one first.
pub struct Tiff<'a> {
    src: &'a dyn RangeRead,
    pub images: Vec<Image>,
}

struct Head {
    bytes: Vec<u8>,
}

impl Head {
    fn u16(&self, at: usize) -> Res<u16> {
        let b = self.bytes.get(at..at + 2).ok_or_else(|| err("short IFD"))?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&self, at: usize) -> Res<u32> {
        let b = self.bytes.get(at..at + 4).ok_or_else(|| err("short IFD"))?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

fn type_size(t: u16) -> Option<usize> {
    match t {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 | 16 => Some(8),
        _ => None,
    }
}

/// A tag's raw value bytes, from the prefix or fetched.
fn value(src: &dyn RangeRead, head: &Head, entry: usize) -> Res<(u16, usize, Vec<u8>)> {
    let ty = head.u16(entry + 2)?;
    let count = head.u32(entry + 4)? as usize;
    let size = type_size(ty).ok_or_else(|| err("tag type"))?;
    let len = size
        .checked_mul(count)
        .filter(|&l| l < (64 << 20))
        .ok_or_else(|| err("tag length"))?;
    if len <= 4 {
        return Ok((ty, count, head.bytes[entry + 8..entry + 8 + len].to_vec()));
    }
    let at = head.u32(entry + 8)? as usize;
    if let Some(b) = head.bytes.get(at..at + len) {
        return Ok((ty, count, b.to_vec()));
    }
    let b = src.read_range(at as u64, len as u64)?;
    if b.len() != len {
        return Err(err("tag past the end"));
    }
    Ok((ty, count, b))
}

fn uints(ty: u16, count: usize, b: &[u8]) -> Res<Vec<u64>> {
    (0..count)
        .map(|i| match ty {
            3 => Ok(u64::from(u16::from_le_bytes([b[2 * i], b[2 * i + 1]]))),
            4 => Ok(u64::from(u32::from_le_bytes([
                b[4 * i],
                b[4 * i + 1],
                b[4 * i + 2],
                b[4 * i + 3],
            ]))),
            16 => {
                let mut a = [0u8; 8];
                a.copy_from_slice(&b[8 * i..8 * i + 8]);
                Ok(u64::from_le_bytes(a))
            }
            _ => Err(err("integer tag type")),
        })
        .collect()
}

fn doubles(b: &[u8]) -> Vec<f64> {
    b.chunks_exact(8)
        .map(|c| {
            let mut a = [0u8; 8];
            a.copy_from_slice(c);
            f64::from_le_bytes(a)
        })
        .collect()
}

impl<'a> Tiff<'a> {
    /// Read the file's image directories.
    pub fn open(src: &'a dyn RangeRead) -> Res<Self> {
        let head = Head {
            bytes: src.read_range(0, PREFIX)?,
        };
        if head.bytes.get(..4) != Some(b"II*\0".as_slice()) {
            return Err(err("not a little-endian classic TIFF"));
        }
        let mut images = Vec::new();
        let mut at = head.u32(4)? as usize;
        while at != 0 && images.len() < 16 {
            let n = usize::from(head.u16(at)?);
            let mut tags = std::collections::HashMap::new();
            for k in 0..n {
                let entry = at + 2 + 12 * k;
                tags.insert(head.u16(entry)?, entry);
            }
            let int = |tag: u16| -> Res<Option<Vec<u64>>> {
                match tags.get(&tag) {
                    Some(&e) => {
                        let (ty, c, b) = value(src, &head, e)?;
                        Ok(Some(uints(ty, c, &b)?))
                    }
                    None => Ok(None),
                }
            };
            let one =
                |tag: u16| -> Res<Option<u64>> { Ok(int(tag)?.and_then(|v| v.first().copied())) };
            let width = one(256)?.ok_or_else(|| err("no width"))?;
            let height = one(257)?.ok_or_else(|| err("no height"))?;
            if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
                return Err(err("image size"));
            }
            if one(277)?.unwrap_or(1) != 1 {
                return Err(err("more than one sample per pixel"));
            }
            if one(284)?.unwrap_or(1) != 1 {
                return Err(err("planar configuration"));
            }
            let bits = one(258)?.unwrap_or(1) as u16;
            let format = one(339)?.unwrap_or(1);
            let float = match (bits, format) {
                (8 | 16, 1) => false,
                (32, 3) => true,
                _ => return Err(err(&format!("{bits}-bit samples of format {format}"))),
            };
            let compression = one(259)?.unwrap_or(1) as u16;
            if !matches!(compression, 1 | 5 | 8 | 32946) {
                return Err(err(&format!("compression {compression}")));
            }
            let predictor = one(317)?.unwrap_or(1) as u16;
            if !matches!(predictor, 1 | 2) {
                return Err(err(&format!("predictor {predictor}")));
            }
            let (chunk, offsets, counts) = match (one(322)?, one(323)?) {
                (Some(tw), Some(th)) => (
                    (tw as usize, th as usize),
                    int(324)?.ok_or_else(|| err("no tile offsets"))?,
                    int(325)?.ok_or_else(|| err("no tile counts"))?,
                ),
                _ => {
                    let rows = one(278)?.unwrap_or(height).min(height);
                    (
                        (width as usize, rows as usize),
                        int(273)?.ok_or_else(|| err("no strip offsets"))?,
                        int(279)?.ok_or_else(|| err("no strip counts"))?,
                    )
                }
            };
            if chunk.0 == 0 || chunk.1 == 0 || chunk.0 > 4096 || chunk.1 > MAX_EDGE as usize {
                return Err(err("chunk size"));
            }
            let across = (width as usize).div_ceil(chunk.0);
            let down = (height as usize).div_ceil(chunk.1);
            if offsets.len() != across * down || counts.len() != offsets.len() {
                return Err(err("chunk table"));
            }
            let dbl = |tag: u16| -> Res<Option<Vec<f64>>> {
                match tags.get(&tag) {
                    Some(&e) => Ok(Some(doubles(&value(src, &head, e)?.2))),
                    None => Ok(None),
                }
            };
            let scale = dbl(33550)?.filter(|s| s.len() >= 2).map(|s| [s[0], s[1]]);
            let tie = dbl(33922)?
                .filter(|t| t.len() >= 6)
                .map(|t| [t[0], t[1], t[3], t[4]]);
            let nodata = match tags.get(&42113) {
                Some(&e) => {
                    let b = value(src, &head, e)?.2;
                    String::from_utf8_lossy(&b)
                        .trim_end_matches('\0')
                        .trim()
                        .parse()
                        .ok()
                }
                None => None,
            };
            images.push(Image {
                width: width as usize,
                height: height as usize,
                bits,
                float,
                compression,
                predictor,
                chunk,
                offsets,
                counts,
                scale,
                tie,
                nodata,
            });
            at = head.u32(at + 2 + 12 * n)? as usize;
        }
        if images.is_empty() {
            return Err(err("no image"));
        }
        Ok(Tiff { src, images })
    }

    /// The samples of image `k` in the pixel window `[x0, x1) x [y0, y1)`
    /// (clipped to the image) as `f32`, row-major; the window's clipped
    /// origin and size with it.
    pub fn window(
        &self,
        k: usize,
        (x0, y0, x1, y1): (usize, usize, usize, usize),
    ) -> Res<(usize, usize, usize, usize, Vec<f32>)> {
        let im = self.images.get(k).ok_or_else(|| err("no such image"))?;
        let (x1, y1) = (x1.min(im.width), y1.min(im.height));
        let (x0, y0) = (x0.min(x1), y0.min(y1));
        let (w, h) = (x1 - x0, y1 - y0);
        let mut out = vec![f32::NAN; w * h];
        if w == 0 || h == 0 {
            return Ok((x0, y0, w, h, out));
        }
        let (cw, ch) = im.chunk;
        let across = im.width.div_ceil(cw);
        let bytes = usize::from(im.bits / 8);
        // The chunks the window needs, fetched in as few ranges as their
        // layout allows: a cloud-optimised file keeps one image's chunks
        // together, so a window is one request, not one per chunk.
        let mut need = Vec::new();
        for cy in y0 / ch..=(y1 - 1) / ch {
            for cx in x0 / cw..=(x1 - 1) / cw {
                let n = cy * across + cx;
                if im.counts[n] > MAX_CHUNK {
                    return Err(err("chunk too large"));
                }
                need.push((cy, cx, n));
            }
        }
        let mut order: Vec<usize> = need.iter().map(|&(_, _, n)| n).collect();
        order.sort_by_key(|&n| im.offsets[n]);
        let mut spans: Vec<(u64, u64, Vec<u8>)> = Vec::new();
        let mut k = 0;
        while k < order.len() {
            let start = im.offsets[order[k]];
            let mut end = start + im.counts[order[k]];
            let mut j = k + 1;
            // Join chunks whose gap is small, up to a span of MAX_CHUNK.
            while j < order.len() {
                let (o, c) = (im.offsets[order[j]], im.counts[order[j]]);
                if o > end + (64 << 10) || o + c - start > MAX_CHUNK {
                    break;
                }
                end = end.max(o + c);
                j += 1;
            }
            let b = self.src.read_range(start, end - start)?;
            if (b.len() as u64) < end - start {
                return Err(err("chunks past the end of the file"));
            }
            spans.push((start, end, b));
            k = j;
        }
        for (cy, cx, n) in need {
            {
                let (off, len) = (im.offsets[n], im.counts[n]);
                let rows = ch.min(im.height - cy * ch);
                // A strip's last chunk is short; a tile is always whole.
                let rows_stored = if cw == im.width { rows } else { ch };
                let raw = spans
                    .iter()
                    .find(|(s, e, _)| off >= *s && off + len <= *e)
                    .map(|(s, _, b)| b[(off - s) as usize..(off - s + len) as usize].to_vec())
                    .ok_or_else(|| err("chunk outside its span"))?;
                let want = cw * rows_stored * bytes;
                let mut data = match im.compression {
                    1 => raw,
                    5 => {
                        let mut d =
                            weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
                        d.decode(&raw).map_err(|e| err(&format!("LZW: {e}")))?
                    }
                    _ => {
                        let mut d = Vec::with_capacity(want);
                        flate2::read::ZlibDecoder::new(&raw[..])
                            .take(want as u64 + 1)
                            .read_to_end(&mut d)?;
                        d
                    }
                };
                if data.len() < want {
                    return Err(err("chunk shorter than its pixels"));
                }
                data.truncate(want);
                if im.predictor == 2 {
                    unpredict(&mut data, cw, bytes);
                }
                for r in 0..rows {
                    let gy = cy * ch + r;
                    if gy < y0 || gy >= y1 {
                        continue;
                    }
                    for c in 0..cw {
                        let gx = cx * cw + c;
                        if gx < x0 || gx >= x1 {
                            continue;
                        }
                        let at = (r * cw + c) * bytes;
                        let v = match (bytes, im.float) {
                            (1, _) => f32::from(data[at]),
                            (2, _) => f32::from(u16::from_le_bytes([data[at], data[at + 1]])),
                            _ => f32::from_le_bytes([
                                data[at],
                                data[at + 1],
                                data[at + 2],
                                data[at + 3],
                            ]),
                        };
                        let nd = im.nodata.is_some_and(|nd| f64::from(v) == nd);
                        out[(gy - y0) * w + gx - x0] = if nd { f32::NAN } else { v };
                    }
                }
            }
        }
        Ok((x0, y0, w, h, out))
    }
}

/// Undo the horizontal predictor in place, row by row of `w` samples of
/// `bytes` each (wrapping integer sums).
fn unpredict(data: &mut [u8], w: usize, bytes: usize) {
    for row in data.chunks_exact_mut(w * bytes) {
        match bytes {
            1 => {
                for i in 1..w {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
            }
            _ => {
                for i in 1..w {
                    let prev = u16::from_le_bytes([row[2 * i - 2], row[2 * i - 1]]);
                    let cur = u16::from_le_bytes([row[2 * i], row[2 * i + 1]]);
                    let v = cur.wrapping_add(prev).to_le_bytes();
                    row[2 * i] = v[0];
                    row[2 * i + 1] = v[1];
                }
            }
        }
    }
}

/// A TIFF writer for tests and fixtures: one image of `u8` or `u16`
/// samples in strips of `rows`, LZW (5) or DEFLATE (8) with the
/// horizontal predictor, georeferenced by a pixel scale and a tie point;
/// optional overviews (halved images) after it.
pub fn write_test_tiff(
    w: usize,
    h: usize,
    samples: &[u16],
    bits: u16,
    compression: u16,
    scale: [f64; 2],
    tie: [f64; 2],
) -> Vec<u8> {
    let bytes = usize::from(bits / 8);
    let rows = 7usize.min(h);
    let mut chunks = Vec::new();
    for y0 in (0..h).step_by(rows) {
        let mut raw = Vec::new();
        for y in y0..(y0 + rows).min(h) {
            let mut prev = 0u16;
            for x in 0..w {
                let v = samples[y * w + x];
                let d = if compression == 8 {
                    v.wrapping_sub(prev)
                } else {
                    v
                };
                prev = v;
                if bytes == 1 {
                    raw.push(d as u8);
                } else {
                    raw.extend_from_slice(&d.to_le_bytes());
                }
            }
        }
        chunks.push(if compression == 5 {
            weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
                .encode(&raw)
                .unwrap_or_default()
        } else {
            use std::io::Write;
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            let _ = z.write_all(&raw);
            z.finish().unwrap_or_default()
        });
    }
    let mut out = b"II*\0".to_vec();
    let ifd_at = 8u32;
    out.extend_from_slice(&ifd_at.to_le_bytes());
    let n_tags = 14u16;
    let ifd_len = 2 + 12 * n_tags as usize + 4;
    let mut extra = Vec::new();
    let extra_at = 8 + ifd_len;
    let nchunks = chunks.len();
    let mut data_at = extra_at + 8 * nchunks + 24 + 48 + 8;
    let mut offsets = Vec::new();
    for c in &chunks {
        offsets.push(data_at as u32);
        data_at += c.len();
    }
    // Offsets, counts, scale, tiepoint, nodata text.
    for o in &offsets {
        extra.extend_from_slice(&o.to_le_bytes());
    }
    for c in &chunks {
        extra.extend_from_slice(&(c.len() as u32).to_le_bytes());
    }
    for v in [scale[0], scale[1], 0.0] {
        extra.extend_from_slice(&v.to_le_bytes());
    }
    for v in [0.0, 0.0, 0.0, tie[0], tie[1], 0.0] {
        extra.extend_from_slice(&v.to_le_bytes());
    }
    extra.extend_from_slice(b"0\0\0\0\0\0\0\0");
    let tag = |t: u16, ty: u16, c: u32, v: u32| -> Vec<u8> {
        let mut e = t.to_le_bytes().to_vec();
        e.extend_from_slice(&ty.to_le_bytes());
        e.extend_from_slice(&c.to_le_bytes());
        e.extend_from_slice(&v.to_le_bytes());
        e
    };
    let x = extra_at as u32;
    let n = nchunks as u32;
    out.extend_from_slice(&n_tags.to_le_bytes());
    for e in [
        tag(256, 4, 1, w as u32),
        tag(257, 4, 1, h as u32),
        tag(258, 3, 1, u32::from(bits)),
        tag(259, 3, 1, u32::from(compression)),
        tag(262, 3, 1, 1),
        tag(273, 4, n, x),
        tag(277, 3, 1, 1),
        tag(278, 4, 1, rows as u32),
        tag(279, 4, n, x + 4 * n),
        tag(284, 3, 1, 1),
        tag(317, 3, 1, if compression == 8 { 2 } else { 1 }),
        tag(33550, 12, 3, x + 8 * n),
        tag(33922, 12, 6, x + 8 * n + 24),
        tag(42113, 2, 2, u32::from_le_bytes(*b"0\0\0\0")),
    ] {
        out.extend_from_slice(&e);
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&extra);
    for c in chunks {
        out.extend_from_slice(&c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(Vec<u8>);
    impl RangeRead for Mem {
        fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
            let s = (offset as usize).min(self.0.len());
            let e = (offset.saturating_add(len) as usize).min(self.0.len());
            Ok(self.0[s..e].to_vec())
        }
    }

    #[test]
    fn lzw_and_deflate_strips_read_back() {
        let (w, h) = (23usize, 17usize);
        let s16: Vec<u16> = (0..w * h).map(|i| (i * 37 % 1000) as u16).collect();
        let s8: Vec<u16> = (0..w * h).map(|i| (i * 13 % 251) as u16).collect();
        for (samples, bits, comp) in [(&s8, 8, 5), (&s16, 16, 5), (&s16, 16, 8), (&s8, 8, 8)] {
            let file = Mem(write_test_tiff(
                w,
                h,
                samples,
                bits,
                comp,
                [0.1, 0.1],
                [-5.0, 31.0],
            ));
            let t = Tiff::open(&file).expect("open");
            let im = &t.images[0];
            assert_eq!((im.width, im.height), (w, h));
            assert_eq!(im.scale, Some([0.1, 0.1]));
            assert_eq!(im.tie, Some([0.0, 0.0, -5.0, 31.0]));
            let (_, _, ww, hh, v) = t.window(0, (3, 2, 20, 16)).expect("window");
            assert_eq!((ww, hh), (17, 14));
            for y in 0..hh {
                for x in 0..ww {
                    let want = samples[(y + 2) * w + x + 3];
                    let got = v[y * ww + x];
                    if want == 0 {
                        assert!(got.is_nan(), "0 is the no-data value");
                    } else {
                        assert_eq!(got, f32::from(want), "bits {bits} comp {comp} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_big_endian_or_foreign_file_is_refused() {
        assert!(Tiff::open(&Mem(b"MM\0*\0\0\0\x08".to_vec())).is_err());
        assert!(Tiff::open(&Mem(b"not a tiff".to_vec())).is_err());
    }
}
