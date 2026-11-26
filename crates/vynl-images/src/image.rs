//! Pure-Rust `RGB24` image buffer, resampling, PPM/BMP/Base64 codecs, procedural
//! cover-art synthesis, and dominant color palette extraction.

use vynl_core::theme::Rgb;

const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Computes a deterministic 64-bit FNV-1a hash of `input`.
#[must_use]
pub const fn fnv1a_64(input: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let bytes = input.as_bytes();
    let mut hash = OFFSET_BASIS;
    let mut idx = 0;
    while idx < bytes.len() {
        hash ^= bytes[idx] as u64;
        hash = hash.wrapping_mul(PRIME);
        idx += 1;
    }
    hash
}

/// Encodes `input` bytes using standard RFC 4648 `Base64` with `=` padding.
#[must_use]
pub fn encode_base64(input: &[u8]) -> String {
    if input.is_empty() {
        return String::new();
    }

    let out_len = input.len().div_ceil(3) * 4;
    let mut out = String::with_capacity(out_len);

    let (chunks, remainder) = input.as_chunks::<3>();

    for chunk in chunks {
        let b0 = usize::from(chunk[0]);
        let b1 = usize::from(chunk[1]);
        let b2 = usize::from(chunk[2]);

        out.push(char::from(BASE64_TABLE[b0 >> 2]));
        out.push(char::from(BASE64_TABLE[((b0 & 0x03) << 4) | (b1 >> 4)]));
        out.push(char::from(BASE64_TABLE[((b1 & 0x0f) << 2) | (b2 >> 6)]));
        out.push(char::from(BASE64_TABLE[b2 & 0x3f]));
    }

    match remainder {
        [b0] => {
            let v0 = usize::from(*b0);
            out.push(char::from(BASE64_TABLE[v0 >> 2]));
            out.push(char::from(BASE64_TABLE[(v0 & 0x03) << 4]));
            out.push('=');
            out.push('=');
        }
        [b0, b1] => {
            let v0 = usize::from(*b0);
            let v1 = usize::from(*b1);
            out.push(char::from(BASE64_TABLE[v0 >> 2]));
            out.push(char::from(BASE64_TABLE[((v0 & 0x03) << 4) | (v1 >> 4)]));
            out.push(char::from(BASE64_TABLE[(v1 & 0x0f) << 2]));
            out.push('=');
        }
        _ => {}
    }

    out
}

/// Uncompressed 24-bit `RGB24` image buffer (3 bytes per pixel in row-major order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Interleaved `R, G, B` pixel bytes (`width * height * 3` bytes).
    pub pixels: Vec<u8>,
}

impl RgbImage {
    /// Creates a validated `RgbImage` from raw `RGB24` bytes.
    ///
    /// # Errors
    /// Returns an error if the buffer dimensions overflow or `pixels.len()` does not
    /// equal `width * height * 3`.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        let expected = expected_byte_len(width, height)?;
        if pixels.len() != expected {
            return Err(format!(
                "invalid RGB24 pixel buffer: expected {expected} bytes for {width}x{height}, got {}",
                pixels.len()
            ));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Creates a solid-color `RgbImage` of dimensions `width` by `height`.
    #[must_use]
    pub fn filled(width: u32, height: u32, color: Rgb) -> Self {
        let Ok(len) = expected_byte_len(width, height) else {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        };
        let mut pixels = Vec::with_capacity(len);
        for _ in 0..(len / 3) {
            pixels.push(color.r);
            pixels.push(color.g);
            pixels.push(color.b);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Returns `true` when either dimension is zero or the pixel buffer is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.pixels.is_empty()
    }

    /// Reads the pixel at `(x, y)` or returns `None` if out of bounds.
    #[must_use]
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Rgb> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = pixel_offset(self.width, x, y)?;
        let red = *self.pixels.get(idx)?;
        let green = *self.pixels.get(idx + 1)?;
        let blue = *self.pixels.get(idx + 2)?;
        Some(Rgb::new(red, green, blue))
    }

    /// Writes `color` at `(x, y)`. Returns `false` if `(x, y)` is out of bounds.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Rgb) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let Some(idx) = pixel_offset(self.width, x, y) else {
            return false;
        };
        if idx + 2 >= self.pixels.len() {
            return false;
        }
        self.pixels[idx] = color.r;
        self.pixels[idx + 1] = color.g;
        self.pixels[idx + 2] = color.b;
        true
    }

    /// Resizes the image to `(width, height)` using nearest-neighbor sampling.
    #[must_use]
    pub fn resize_nearest(&self, width: u32, height: u32) -> Self {
        if self.is_empty() || width == 0 || height == 0 {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        }
        if self.width == width && self.height == height {
            return self.clone();
        }
        let Ok(out_len) = expected_byte_len(width, height) else {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        };

        let mut pixels = Vec::with_capacity(out_len);
        let source_width = u64::from(self.width);
        let source_height = u64::from(self.height);
        let target_width = u64::from(width);
        let target_height = u64::from(height);

        for row in 0..target_height {
            let src_row = ((row * source_height) / target_height).min(source_height - 1);
            let Ok(sy) = u32::try_from(src_row) else {
                break;
            };
            for col in 0..target_width {
                let src_col = ((col * source_width) / target_width).min(source_width - 1);
                let Ok(sx) = u32::try_from(src_col) else {
                    break;
                };
                let px = self.get_pixel(sx, sy).unwrap_or(Rgb::new(0, 0, 0));
                pixels.push(px.r);
                pixels.push(px.g);
                pixels.push(px.b);
            }
        }

        Self {
            width,
            height,
            pixels,
        }
    }

    /// Resizes the image to `(width, height)` using pixel-center bilinear interpolation.
    #[must_use]
    pub fn resize_bilinear(&self, width: u32, height: u32) -> Self {
        if self.is_empty() || width == 0 || height == 0 {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        }
        if self.width == width && self.height == height {
            return self.clone();
        }
        let Ok(out_len) = expected_byte_len(width, height) else {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        };

        let mut pixels = Vec::with_capacity(out_len);
        let source_width = f64::from(self.width);
        let source_height = f64::from(self.height);
        let target_width = f64::from(width);
        let target_height = f64::from(height);
        let max_col = f64::from(self.width - 1);
        let max_row = f64::from(self.height - 1);

        for row in 0..height {
            let sample_y =
                ((f64::from(row) + 0.5) * source_height / target_height - 0.5).clamp(0.0, max_row);
            let y0 = clamp_f64_to_u32(sample_y.floor(), self.height - 1);
            let y1 = (y0 + 1).min(self.height - 1);
            let ty = sample_y - f64::from(y0);

            for col in 0..width {
                let sample_x = ((f64::from(col) + 0.5) * source_width / target_width - 0.5)
                    .clamp(0.0, max_col);
                let x0 = clamp_f64_to_u32(sample_x.floor(), self.width - 1);
                let x1 = (x0 + 1).min(self.width - 1);
                let tx = sample_x - f64::from(x0);

                let c00 = self.get_pixel(x0, y0).unwrap_or(Rgb::new(0, 0, 0));
                let c10 = self.get_pixel(x1, y0).unwrap_or(Rgb::new(0, 0, 0));
                let c01 = self.get_pixel(x0, y1).unwrap_or(Rgb::new(0, 0, 0));
                let c11 = self.get_pixel(x1, y1).unwrap_or(Rgb::new(0, 0, 0));

                let r = bilerp_channel(c00.r, c10.r, c01.r, c11.r, tx, ty);
                let g = bilerp_channel(c00.g, c10.g, c01.g, c11.g, tx, ty);
                let b = bilerp_channel(c00.b, c10.b, c01.b, c11.b, tx, ty);

                pixels.push(r);
                pixels.push(g);
                pixels.push(b);
            }
        }

        Self {
            width,
            height,
            pixels,
        }
    }

    /// Computes aspect-ratio-preserving pixel dimensions that fit inside
    /// `max_width x max_height`.
    #[must_use]
    pub fn fitted_dimensions(&self, max_width: u32, max_height: u32) -> (u32, u32) {
        if self.is_empty() || max_width == 0 || max_height == 0 {
            return (0, 0);
        }
        let scale_x = f64::from(max_width) / f64::from(self.width);
        let scale_y = f64::from(max_height) / f64::from(self.height);
        let scale = scale_x.min(scale_y);

        let target_w =
            clamp_f64_to_u32((f64::from(self.width) * scale).round().max(1.0), max_width);
        let target_h = clamp_f64_to_u32(
            (f64::from(self.height) * scale).round().max(1.0),
            max_height,
        );
        (target_w, target_h)
    }

    /// Resizes the image with bilinear interpolation while preserving aspect ratio
    /// to fit within a terminal cell area of `cols x rows` cells sized `cell_px_w x cell_px_h`.
    #[must_use]
    pub fn fit_to_cells(&self, cols: u16, rows: u16, cell_px_w: u16, cell_px_h: u16) -> Self {
        let max_w = u32::from(cols).saturating_mul(u32::from(cell_px_w));
        let max_h = u32::from(rows).saturating_mul(u32::from(cell_px_h));
        let (target_w, target_h) = self.fitted_dimensions(max_w, max_h);
        if target_w == 0 || target_h == 0 {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        }
        self.resize_bilinear(target_w, target_h)
    }

    /// Encodes the image as a binary `Netpbm` PPM (`P6`) byte buffer.
    #[must_use]
    pub fn encode_ppm(&self) -> Vec<u8> {
        let header = format!("P6\n{} {}\n255\n", self.width, self.height);
        let mut out = Vec::with_capacity(header.len() + self.pixels.len());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&self.pixels);
        out
    }

    /// Decodes a binary `Netpbm` PPM (`P6`) byte buffer into an `RgbImage`.
    ///
    /// # Errors
    /// Returns an error if the PPM header is malformed, uses an unsupported maxval,
    /// or has a pixel payload length that does not match `width * height * 3`.
    pub fn decode_ppm(bytes: &[u8]) -> Result<Self, String> {
        let mut cursor = 0usize;
        let magic = read_ppm_token(bytes, &mut cursor)?;
        if magic != b"P6" {
            return Err(format!(
                "unsupported PPM magic `{}` (expected `P6`)",
                String::from_utf8_lossy(magic)
            ));
        }

        let width = parse_ppm_u32(read_ppm_token(bytes, &mut cursor)?, "width")?;
        let height = parse_ppm_u32(read_ppm_token(bytes, &mut cursor)?, "height")?;
        let maxval = parse_ppm_u32(read_ppm_token(bytes, &mut cursor)?, "maxval")?;
        if maxval != 255 {
            return Err(format!("unsupported PPM maxval {maxval} (expected 255)"));
        }

        // Netpbm P6 specifies a single whitespace byte immediately following maxval.
        let Some(&sep) = bytes.get(cursor) else {
            return Err(String::from("truncated PPM header before pixel data"));
        };
        if !sep.is_ascii_whitespace() {
            return Err(String::from(
                "expected whitespace separator after PPM maxval",
            ));
        }
        cursor += 1;

        let expected = expected_byte_len(width, height)?;
        let payload = bytes
            .get(cursor..)
            .ok_or_else(|| String::from("truncated PPM payload"))?;
        if payload.len() != expected {
            return Err(format!(
                "PPM payload length mismatch: expected {expected} bytes for {width}x{height}, got {}",
                payload.len()
            ));
        }

        Ok(Self {
            width,
            height,
            pixels: payload.to_vec(),
        })
    }

    /// Decodes an encoded image payload (`PPM` `P6`, `PNG`, `JPEG`, or `WebP`) into
    /// an uncompressed [`RgbImage`].
    ///
    /// # Errors
    /// Returns a clear English error message if decoding fails or if the decoded
    /// image has zero width or height.
    pub fn decode_encoded(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() {
            return Err(String::from("cannot decode empty image payload"));
        }

        let decoded = if bytes.starts_with(b"P6") {
            Self::decode_ppm(bytes)?
        } else {
            let dynamic = image::load_from_memory(bytes)
                .map_err(|err| format!("failed to decode encoded image: {err}"))?;
            let rgb = dynamic.to_rgb8();
            Self::new(rgb.width(), rgb.height(), rgb.into_raw())?
        };

        if decoded.width == 0 || decoded.height == 0 {
            return Err(format!(
                "decoded image has zero dimension: {}x{}",
                decoded.width, decoded.height
            ));
        }

        Ok(decoded)
    }

    /// Encodes the image as an uncompressed 24-bit Windows BMP (`BGR24`, bottom-up)
    /// suitable for `iTerm2` OSC 1337 inline image payloads.
    #[must_use]
    pub fn encode_bmp(&self) -> Vec<u8> {
        if self.is_empty() {
            return Vec::new();
        }
        let Ok(w_usize) = usize::try_from(self.width) else {
            return Vec::new();
        };
        let Ok(h_usize) = usize::try_from(self.height) else {
            return Vec::new();
        };
        let row_raw = w_usize.saturating_mul(3);
        let row_stride = (row_raw + 3) & !3;
        let pad_bytes = row_stride - row_raw;
        let pixel_bytes = row_stride.saturating_mul(h_usize);

        let Ok(pixel_bytes_u32) = u32::try_from(pixel_bytes) else {
            return Vec::new();
        };
        let Some(file_size) = pixel_bytes_u32.checked_add(54) else {
            return Vec::new();
        };
        let Ok(width_i32) = i32::try_from(self.width) else {
            return Vec::new();
        };
        let Ok(height_i32) = i32::try_from(self.height) else {
            return Vec::new();
        };

        let mut out = Vec::with_capacity(54 + pixel_bytes);
        // BITMAPFILEHEADER (14 bytes)
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&file_size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());

        // BITMAPINFOHEADER (40 bytes)
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&width_i32.to_le_bytes());
        out.extend_from_slice(&height_i32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&pixel_bytes_u32.to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());

        // Pixel rows stored bottom-up in BGR order.
        for y in (0..self.height).rev() {
            for x in 0..self.width {
                let px = self.get_pixel(x, y).unwrap_or(Rgb::new(0, 0, 0));
                out.push(px.b);
                out.push(px.g);
                out.push(px.r);
            }
            out.extend(std::iter::repeat_n(0u8, pad_bytes));
        }

        out
    }

    /// Extracts up to `max_colors` visually distinct dominant colors for adaptive
    /// cover-art theming.
    ///
    /// Pixels are bucketed into a 16x16x16 color histogram, scored by pixel coverage
    /// with a gentle saturation preference so vibrant accents surface ahead of muted
    /// borders, and filtered for perceptual diversity.
    #[must_use]
    pub fn extract_dominant_palette(&self, max_colors: usize) -> Vec<Rgb> {
        const BUCKETS: usize = 16 * 16 * 16;
        const MIN_DIST_SQ: i32 = 42 * 42;

        if max_colors == 0 || self.is_empty() {
            return Vec::new();
        }

        let mut counts = vec![0u64; BUCKETS];
        let mut sum_r = vec![0u64; BUCKETS];
        let mut sum_g = vec![0u64; BUCKETS];
        let mut sum_b = vec![0u64; BUCKETS];

        for chunk in self.pixels.as_chunks::<3>().0 {
            let r = chunk[0];
            let g = chunk[1];
            let b = chunk[2];
            let idx = (usize::from(r >> 4) << 8) | (usize::from(g >> 4) << 4) | usize::from(b >> 4);
            counts[idx] += 1;
            sum_r[idx] += u64::from(r);
            sum_g[idx] += u64::from(g);
            sum_b[idx] += u64::from(b);
        }

        let mut candidates: Vec<(f64, u64, Rgb)> = Vec::new();
        for idx in 0..BUCKETS {
            let count = counts[idx];
            if count == 0 {
                continue;
            }
            let avg_r = u8::try_from(sum_r[idx] / count).unwrap_or(255);
            let avg_g = u8::try_from(sum_g[idx] / count).unwrap_or(255);
            let avg_b = u8::try_from(sum_b[idx] / count).unwrap_or(255);
            let color = Rgb::new(avg_r, avg_g, avg_b);

            let max_c = avg_r.max(avg_g).max(avg_b);
            let min_c = avg_r.min(avg_g).min(avg_b);
            let saturation = if max_c == 0 {
                0.0
            } else {
                f64::from(max_c - min_c) / f64::from(max_c)
            };
            let score = u64_to_f64_lossy(count) * (1.0 + 0.85 * saturation);
            candidates.push((score, count, color));
        }

        candidates.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| (a.2.r, a.2.g, a.2.b).cmp(&(b.2.r, b.2.g, b.2.b)))
        });

        let mut palette = Vec::with_capacity(max_colors.min(candidates.len()));
        for &(_, _, color) in &candidates {
            if palette.len() >= max_colors {
                break;
            }
            let too_close = palette
                .iter()
                .any(|&existing| rgb_distance_sq(existing, color) < MIN_DIST_SQ);
            if !too_close {
                palette.push(color);
            }
        }

        palette
    }
}

/// Procedurally generates colorful vinyl/album cover artwork deterministically from `seed`.
///
/// Used for offline demos, integration tests, and fallback artwork without external assets.
#[must_use]
pub fn generate_demo_cover(seed: &str, size: u32) -> RgbImage {
    if size == 0 {
        return RgbImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    }

    let hash = fnv1a_64(seed);
    let base_hue = f64::from((hash & 0xffff) as u16) / 65535.0 * 360.0;
    let alt_hue = (base_hue + 45.0 + f64::from(((hash >> 16) & 0xff) as u8) / 255.0 * 95.0) % 360.0;
    let label_hue = (base_hue + 175.0) % 360.0;
    let accent_hue = (base_hue + 250.0) % 360.0;

    let bg_a = hsv_to_rgb(base_hue, 0.78, 0.90);
    let bg_b = hsv_to_rgb(alt_hue, 0.85, 0.58);
    let label_color = hsv_to_rgb(label_hue, 0.82, 0.92);
    let geo_color = hsv_to_rgb(accent_hue, 0.72, 0.96);

    let variant = (hash >> 32) % 3;
    let inv_size = 1.0 / f64::from(size);
    let Ok(buf_len) = expected_byte_len(size, size) else {
        return RgbImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    };
    let mut pixels = Vec::with_capacity(buf_len);

    for y in 0..size {
        let ny = (f64::from(y) + 0.5) * inv_size;
        let dy = ny - 0.5;
        for x in 0..size {
            let nx = (f64::from(x) + 0.5) * inv_size;
            let dx = nx - 0.5;
            let dist = dx.hypot(dy);

            let grad_t = (0.6 * nx + 0.4 * ny).clamp(0.0, 1.0);
            let mut color = lerp_rgb(bg_a, bg_b, grad_t);

            // Seed-driven geometric sleeve motif behind the vinyl record.
            let in_motif = match variant {
                0 => (nx + ny) > 1.28,
                1 => (nx - ny).abs() < 0.12 && dist > 0.36,
                _ => nx > 0.68 && ny < 0.34,
            };
            if in_motif {
                color = lerp_rgb(color, geo_color, 0.65);
            }

            // Central vinyl record disc with concentric grooves, center label, and spindle hole.
            if dist < 0.37 {
                if dist < 0.035 {
                    // Spindle hole
                    color = Rgb::new(16, 16, 20);
                } else if dist < 0.145 {
                    // Center record label with subtle inner ring
                    let ring = if (dist - 0.11).abs() < 0.012 {
                        0.25
                    } else {
                        0.0
                    };
                    color = lerp_rgb(label_color, Rgb::new(250, 250, 250), ring);
                } else {
                    // Dark vinyl with groove shimmer
                    let groove = ((dist * 110.0).sin() * 0.5 + 0.5) * 14.0;
                    let sheen = ((dx * dy) * 12.0).clamp(-0.5, 0.5).abs() * 30.0;
                    let base = clamp_f64_to_u8(20.0 + groove + sheen);
                    color = Rgb::new(base, base, base.saturating_add(4));
                }
            } else if dist < 0.388 {
                // Crisp vinyl rim highlight
                color = lerp_rgb(color, Rgb::new(245, 245, 250), 0.35);
            }

            pixels.push(color.r);
            pixels.push(color.g);
            pixels.push(color.b);
        }
    }

    RgbImage {
        width: size,
        height: size,
        pixels,
    }
}

fn expected_byte_len(width: u32, height: u32) -> Result<usize, String> {
    let w = usize::try_from(width).map_err(|_| String::from("image width exceeds usize"))?;
    let h = usize::try_from(height).map_err(|_| String::from("image height exceeds usize"))?;
    w.checked_mul(h)
        .and_then(|px| px.checked_mul(3))
        .ok_or_else(|| format!("image dimensions {width}x{height} overflow byte size"))
}

fn pixel_offset(width: u32, x: u32, y: u32) -> Option<usize> {
    let w = usize::try_from(width).ok()?;
    let xu = usize::try_from(x).ok()?;
    let yu = usize::try_from(y).ok()?;
    yu.checked_mul(w)?.checked_add(xu)?.checked_mul(3)
}

fn bilerp_channel(c00: u8, c10: u8, c01: u8, c11: u8, tx: f64, ty: f64) -> u8 {
    let top = f64::from(c00) + (f64::from(c10) - f64::from(c00)) * tx;
    let bot = f64::from(c01) + (f64::from(c11) - f64::from(c01)) * tx;
    clamp_f64_to_u8(top + (bot - top) * ty)
}

fn lerp_rgb(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let f = t.clamp(0.0, 1.0);
    let mix = |c0: u8, c1: u8| -> u8 {
        clamp_f64_to_u8(f64::from(c0) + (f64::from(c1) - f64::from(c0)) * f)
    };
    Rgb::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

fn hsv_to_rgb(hue_deg: f64, sat: f64, val: f64) -> Rgb {
    let h = hue_deg.rem_euclid(360.0) / 60.0;
    let c = val * sat;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = val - c;

    let (r1, g1, b1) = if h < 1.0 {
        (c, x, 0.0)
    } else if h < 2.0 {
        (x, c, 0.0)
    } else if h < 3.0 {
        (0.0, c, x)
    } else if h < 4.0 {
        (0.0, x, c)
    } else if h < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    Rgb::new(
        clamp_f64_to_u8((r1 + m) * 255.0),
        clamp_f64_to_u8((g1 + m) * 255.0),
        clamp_f64_to_u8((b1 + m) * 255.0),
    )
}

fn rgb_distance_sq(a: Rgb, b: Rgb) -> i32 {
    let dr = i32::from(a.r) - i32::from(b.r);
    let dg = i32::from(a.g) - i32::from(b.g);
    let db = i32::from(a.b) - i32::from(b.b);
    dr * dr + dg * dg + db * db
}

fn clamp_f64_to_u8(value: f64) -> u8 {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = value.round().clamp(0.0, 255.0) as u8;
    out
}

fn clamp_f64_to_u32(value: f64, max: u32) -> u32 {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = value.clamp(0.0, f64::from(max)) as u32;
    out
}

fn u64_to_f64_lossy(value: u64) -> f64 {
    #[expect(clippy::cast_precision_loss)]
    let out = value as f64;
    out
}

fn read_ppm_token<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], String> {
    while *cursor < bytes.len() {
        let byte = bytes[*cursor];
        if byte.is_ascii_whitespace() {
            *cursor += 1;
        } else if byte == b'#' {
            while *cursor < bytes.len() && bytes[*cursor] != b'\n' {
                *cursor += 1;
            }
        } else {
            break;
        }
    }

    let start = *cursor;
    while *cursor < bytes.len() && !bytes[*cursor].is_ascii_whitespace() && bytes[*cursor] != b'#' {
        *cursor += 1;
    }

    if start == *cursor {
        return Err(String::from("unexpected end of PPM header"));
    }
    Ok(&bytes[start..*cursor])
}

fn parse_ppm_u32(token: &[u8], field: &str) -> Result<u32, String> {
    let text = std::str::from_utf8(token).map_err(|_| format!("non-UTF-8 PPM {field} token"))?;
    text.parse::<u32>()
        .map_err(|_| format!("invalid PPM {field} `{text}`"))
}

#[cfg(test)]
mod tests {
    use vynl_core::theme::Rgb;

    use super::{RgbImage, encode_base64, fnv1a_64, generate_demo_cover};

    #[test]
    fn base64_encodes_rfc4648_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn ppm_round_trips_losslessly_and_supports_comments() -> Result<(), String> {
        let img = generate_demo_cover("album:dark-side", 16);
        let encoded = img.encode_ppm();
        let decoded = RgbImage::decode_ppm(&encoded)?;
        assert_eq!(decoded, img);

        // Also verify header comment parsing per Netpbm P6 specification.
        let mut commented = b"P6\n# generated by vynl\n2 1\n# maxval\n255\n".to_vec();
        commented.extend_from_slice(&[10, 20, 30, 200, 210, 220]);
        let parsed = RgbImage::decode_ppm(&commented)?;
        assert_eq!(parsed.width, 2);
        assert_eq!(parsed.height, 1);
        assert_eq!(parsed.get_pixel(0, 0), Some(Rgb::new(10, 20, 30)));
        assert_eq!(parsed.get_pixel(1, 0), Some(Rgb::new(200, 210, 220)));
        Ok(())
    }

    #[test]
    fn bilinear_resize_smoothly_interpolates_gradients() -> Result<(), String> {
        // 2x1 horizontal black-to-white ramp resized to 4x1 should produce smooth intermediate tones.
        let src = RgbImage::new(2, 1, vec![0, 0, 0, 255, 255, 255])?;
        let dst = src.resize_bilinear(4, 1);
        assert_eq!(dst.width, 4);
        assert_eq!(dst.height, 1);

        let p0 = dst
            .get_pixel(0, 0)
            .ok_or_else(|| String::from("missing p0"))?;
        let p1 = dst
            .get_pixel(1, 0)
            .ok_or_else(|| String::from("missing p1"))?;
        let p2 = dst
            .get_pixel(2, 0)
            .ok_or_else(|| String::from("missing p2"))?;
        let p3 = dst
            .get_pixel(3, 0)
            .ok_or_else(|| String::from("missing p3"))?;
        assert!(p0.r < p1.r && p1.r < p2.r && p2.r < p3.r);
        assert_eq!(p0, Rgb::new(0, 0, 0));
        assert_eq!(p3, Rgb::new(255, 255, 255));
        Ok(())
    }

    #[test]
    fn fit_to_cells_preserves_aspect_ratio() {
        let wide = RgbImage::filled(200, 100, Rgb::new(12, 34, 56));
        let fitted = wide.fit_to_cells(10, 10, 8, 16); // bounding box 80x160
        assert_eq!(fitted.width, 80);
        assert_eq!(fitted.height, 40);

        let tall = RgbImage::filled(100, 200, Rgb::new(12, 34, 56));
        let fitted_tall = tall.fit_to_cells(20, 5, 8, 16); // bounding box 160x80
        assert_eq!(fitted_tall.width, 40);
        assert_eq!(fitted_tall.height, 80);
    }

    #[test]
    fn dominant_palette_extracts_distinct_prominent_colors() {
        // 8x4 image: 20 coral pixels + 12 teal pixels.
        let coral = Rgb::new(240, 80, 70);
        let teal = Rgb::new(30, 195, 185);
        let mut img = RgbImage::filled(8, 4, coral);
        for y in 0..4 {
            for x in 5..8 {
                assert!(img.set_pixel(x, y, teal));
            }
        }
        let palette = img.extract_dominant_palette(3);
        assert_eq!(palette.len(), 2);
        assert_eq!(palette[0], coral);
        assert_eq!(palette[1], teal);

        // Demo covers should also yield rich multi-color palettes.
        let demo = generate_demo_cover("spotify:album:42", 48);
        let demo_palette = demo.extract_dominant_palette(4);
        assert!(demo_palette.len() >= 2);
        assert_ne!(fnv1a_64("a"), fnv1a_64("b"));
    }

    #[test]
    fn bmp_encoding_writes_valid_24bit_header() {
        let img = RgbImage::filled(3, 2, Rgb::new(10, 20, 30));
        let bmp = img.encode_bmp();
        assert_eq!(&bmp[0..2], b"BM");
        // Row stride for 3px * 3B = 9 -> padded to 12 bytes; 2 rows = 24 bytes + 54 header = 78.
        assert_eq!(bmp.len(), 78);
    }

    #[test]
    fn nearest_resize_preserves_crisp_quadrants_and_rejects_bad_ppm() -> Result<(), String> {
        let src = RgbImage::new(
            2,
            2,
            vec![
                255, 0, 0, 0, 255, 0, // top row: red, green
                0, 0, 255, 255, 255, 0, // bottom row: blue, yellow
            ],
        )?;
        let dst = src.resize_nearest(4, 4);
        assert_eq!(dst.get_pixel(0, 0), Some(Rgb::new(255, 0, 0)));
        assert_eq!(dst.get_pixel(3, 0), Some(Rgb::new(0, 255, 0)));
        assert_eq!(dst.get_pixel(0, 3), Some(Rgb::new(0, 0, 255)));
        assert_eq!(dst.get_pixel(3, 3), Some(Rgb::new(255, 255, 0)));

        assert!(RgbImage::decode_ppm(b"P3\n1 1\n255\n0 0 0").is_err());
        assert!(RgbImage::decode_ppm(b"P6\n2 2\n255\n\x00\x00").is_err());
        assert!(RgbImage::decode_ppm(b"P6\n1 1\n65535\n\x00\x00\x00").is_err());
        Ok(())
    }

    #[test]
    fn decode_encoded_decodes_png_jpeg_webp_and_ppm_and_rejects_invalid() -> Result<(), String> {
        use std::io::Cursor;

        // Build a 4x4 reference image with four crisp color quadrants.
        let mut raw_img = image::RgbImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                let px = match (x < 2, y < 2) {
                    (true, true) => image::Rgb([220, 40, 50]),
                    (false, true) => image::Rgb([40, 200, 90]),
                    (true, false) => image::Rgb([50, 90, 230]),
                    (false, false) => image::Rgb([240, 210, 45]),
                };
                raw_img.put_pixel(x, y, px);
            }
        }
        let dynamic = image::DynamicImage::ImageRgb8(raw_img);

        // PNG (lossless)
        let mut png_bytes = Vec::new();
        dynamic
            .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .map_err(|err| format!("failed to encode test PNG: {err}"))?;
        let png_decoded = RgbImage::decode_encoded(&png_bytes)?;
        assert_eq!(png_decoded.width, 4);
        assert_eq!(png_decoded.height, 4);
        assert_eq!(png_decoded.get_pixel(0, 0), Some(Rgb::new(220, 40, 50)));
        assert_eq!(png_decoded.get_pixel(3, 0), Some(Rgb::new(40, 200, 90)));
        assert_eq!(png_decoded.get_pixel(0, 3), Some(Rgb::new(50, 90, 230)));
        assert_eq!(png_decoded.get_pixel(3, 3), Some(Rgb::new(240, 210, 45)));

        // WebP (lossless in image 0.25)
        let mut webp_bytes = Vec::new();
        dynamic
            .write_to(&mut Cursor::new(&mut webp_bytes), image::ImageFormat::WebP)
            .map_err(|err| format!("failed to encode test WebP: {err}"))?;
        let webp_decoded = RgbImage::decode_encoded(&webp_bytes)?;
        assert_eq!(webp_decoded, png_decoded);

        // JPEG (lossy — verify dimensions and pixel fidelity on a solid 8x8 color tile)
        let solid = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 80, 40]));
        let mut jpeg_bytes = Vec::new();
        image::DynamicImage::ImageRgb8(solid)
            .write_to(&mut Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg)
            .map_err(|err| format!("failed to encode test JPEG: {err}"))?;
        let jpeg_decoded = RgbImage::decode_encoded(&jpeg_bytes)?;
        assert_eq!(jpeg_decoded.width, 8);
        assert_eq!(jpeg_decoded.height, 8);
        let center = jpeg_decoded
            .get_pixel(4, 4)
            .ok_or_else(|| String::from("missing JPEG center pixel"))?;
        assert!(center.r.abs_diff(200) <= 10);
        assert!(center.g.abs_diff(80) <= 10);
        assert!(center.b.abs_diff(40) <= 10);

        // PPM P6 delegation
        let ppm_bytes = png_decoded.encode_ppm();
        let ppm_decoded = RgbImage::decode_encoded(&ppm_bytes)?;
        assert_eq!(ppm_decoded, png_decoded);

        // Error cases: empty, invalid bytes, zero-dimension PPM
        assert!(RgbImage::decode_encoded(b"").is_err());
        assert!(RgbImage::decode_encoded(b"definitely-not-an-image").is_err());
        assert!(RgbImage::decode_encoded(b"P6\n0 0\n255\n").is_err());
        Ok(())
    }
}
