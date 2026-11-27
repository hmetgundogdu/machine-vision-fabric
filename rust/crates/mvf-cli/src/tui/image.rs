//! Preview images in the terminal (#35).
//!
//! The runtime's previews are PNGs with stored deflate blocks (`mvf_management::preview::png`),
//! which this module decodes without a codec. It refuses anything else rather than guessing.
//!
//! Pixels become terminal cells, one ASCII character each, so no glyph has an ambiguous width:
//! - **Truecolor:** a space with a 24-bit background, two source rows averaged per cell (terminal
//!   cells are about twice as tall as wide).
//! - **Otherwise:** a grey ramp of ASCII characters.

/// A decoded image: 8-bit grey (1), RGB (3) or RGBA (4) channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Channels per pixel.
    pub channels: usize,
    /// Row-major pixels.
    pub pixels: Vec<u8>,
}

impl Image {
    /// RGB of pixel (x, y); grey is replicated, alpha ignored.
    pub fn rgb(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let at = (y * self.width + x) * self.channels;
        match self.channels {
            1 => (self.pixels[at], self.pixels[at], self.pixels[at]),
            _ => (self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]),
        }
    }
}

/// Decodes a PNG the runtime wrote: 8-bit grey, RGB or RGBA, stored deflate, filter 0.
pub fn decode_png(png: &[u8]) -> Result<Image, String> {
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("not a PNG".into());
    }
    let (mut at, mut ihdr, mut z) = (8usize, None, Vec::new());
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().map_err(|_| "chunk")?) as usize;
        let kind = &png[at + 4..at + 8];
        let data = png.get(at + 8..at + 8 + len).ok_or("truncated chunk")?;
        match kind {
            b"IHDR" if len >= 13 => ihdr = Some(data.to_vec()),
            b"IDAT" => z.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    let h = ihdr.ok_or("no IHDR")?;
    let width = u32::from_be_bytes(h[0..4].try_into().map_err(|_| "IHDR")?) as usize;
    let height = u32::from_be_bytes(h[4..8].try_into().map_err(|_| "IHDR")?) as usize;
    let channels = match (h[8], h[9]) {
        (8, 0) => 1,
        (8, 2) => 3,
        (8, 6) => 4,
        (depth, colour) => {
            return Err(format!(
                "{depth}-bit, colour type {colour} (8-bit grey, RGB, RGBA only)"
            ));
        }
    };
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(format!("{width}x{height} is outside the preview bounds"));
    }
    // zlib: 2-byte header, then stored blocks only.
    let mut raw = Vec::with_capacity((width * channels + 1) * height);
    let mut i = 2usize;
    loop {
        let head = *z.get(i).ok_or("truncated deflate")?;
        if head & 0b110 != 0 {
            return Err(
                "compressed deflate blocks (only the runtime's stored PNGs are shown)".into(),
            );
        }
        let n = u16::from_le_bytes([
            *z.get(i + 1).ok_or("truncated deflate")?,
            *z.get(i + 2).ok_or("truncated deflate")?,
        ]) as usize;
        raw.extend_from_slice(z.get(i + 5..i + 5 + n).ok_or("truncated deflate")?);
        i += 5 + n;
        if head & 1 == 1 {
            break;
        }
    }
    let row = width * channels + 1;
    if raw.len() < row * height {
        return Err("truncated image data".into());
    }
    let mut pixels = Vec::with_capacity(width * channels * height);
    for r in raw.chunks(row).take(height) {
        if r[0] != 0 {
            return Err(format!("PNG filter {} (only filter 0)", r[0]));
        }
        pixels.extend_from_slice(&r[1..]);
    }
    Ok(Image {
        width,
        height,
        channels,
        pixels,
    })
}

/// Whether the terminal says it shows 24-bit colour.
pub fn truecolor() -> bool {
    let c = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    c.contains("truecolor") || c.contains("24bit") || std::env::var_os("WT_SESSION").is_some()
}

/// One terminal cell of a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixel {
    /// A space on this background colour.
    Colour(u8, u8, u8),
    /// An ASCII character for this brightness.
    Ramp(char),
}

const RAMP: &[u8] = b" .:-=+*#%@";

/// The image fitted into `cols` × `rows` cells, aspect kept (a cell is two pixels tall).
pub fn cells(img: &Image, cols: usize, rows: usize, colour: bool) -> Vec<Vec<Pixel>> {
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    // Cell aspect 1:2, so the image is `width` cells wide and `height / 2` rows tall at scale 1.
    let scale = (img.width as f64 / cols as f64)
        .max(img.height as f64 / (rows as f64 * 2.0))
        .max(f64::MIN_POSITIVE);
    let (w, h) = (
        ((img.width as f64 / scale).floor() as usize).clamp(1, cols),
        ((img.height as f64 / scale / 2.0).floor() as usize).clamp(1, rows),
    );
    (0..h)
        .map(|cy| {
            (0..w)
                .map(|cx| {
                    let sx = (cx * img.width / w).min(img.width - 1);
                    let y0 = (cy * img.height / h).min(img.height - 1);
                    let y1 = ((cy * img.height / h) + (img.height / h / 2).max(1) - 1)
                        .min(img.height - 1);
                    let (a, b) = (img.rgb(sx, y0), img.rgb(sx, y1));
                    let (r, g, bl) = (
                        ((u16::from(a.0) + u16::from(b.0)) / 2) as u8,
                        ((u16::from(a.1) + u16::from(b.1)) / 2) as u8,
                        ((u16::from(a.2) + u16::from(b.2)) / 2) as u8,
                    );
                    if colour {
                        Pixel::Colour(r, g, bl)
                    } else {
                        let luma =
                            (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(bl) * 114) / 1000;
                        Pixel::Ramp(RAMP[(luma as usize * (RAMP.len() - 1)) / 255] as char)
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtimes_own_previews_decode_pixel_exact() {
        let pixels: Vec<u8> = (0..4 * 3 * 3).map(|i| (i * 7) as u8).collect();
        let png = mvf_management::preview::png(4, 3, 3, &pixels);
        let img = decode_png(&png).expect("decode");
        assert_eq!((img.width, img.height, img.channels), (4, 3, 3));
        assert_eq!(img.pixels, pixels);
        let grey =
            decode_png(&mvf_management::preview::png(2, 2, 1, &[0, 85, 170, 255])).expect("grey");
        assert_eq!(grey.rgb(1, 1), (255, 255, 255));
    }

    #[test]
    fn other_data_is_refused_not_guessed() {
        assert!(decode_png(b"BM\x00\x00").is_err());
        let mut png = mvf_management::preview::png(2, 2, 3, &[0; 12]);
        png.truncate(png.len() - 20);
        assert!(decode_png(&png).is_err(), "truncated");
    }

    #[test]
    fn pictures_fit_the_panel_and_use_only_ascii() {
        let img = decode_png(&mvf_management::preview::png(
            160,
            120,
            3,
            &vec![200; 160 * 120 * 3],
        ))
        .expect("img");
        let art = cells(&img, 40, 10, false);
        assert!(art.len() <= 10 && art.iter().all(|r| r.len() <= 40));
        assert!(
            art.iter()
                .flatten()
                .all(|p| matches!(p, Pixel::Ramp(c) if c.is_ascii()))
        );
        let colour = cells(&img, 40, 10, true);
        assert!(matches!(colour[0][0], Pixel::Colour(200, 200, 200)));
    }
}
