//! Terminal graphics protocol detection and encoders for Kitty, `iTerm2`, Sixel,
//! and 24-bit `TrueColor` Unicode halfblocks.

use std::fmt::Write as _;

use vynl_core::config::{ImageProtocolPreference, ListThumbnailPolicy};
use vynl_core::theme::Rgb;

use crate::image::{RgbImage, encode_base64};

const KITTY_CHUNK_SIZE: usize = 4096;

/// Resolved terminal image protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageProtocol {
    /// Kitty Graphics Protocol (Kitty, Ghostty, `WezTerm`).
    Kitty,
    /// `iTerm2` Inline Images (`iTerm2`, `WezTerm`, Konsole).
    ITerm2,
    /// Sixel graphics (Foot, `XTerm`, Windows Terminal).
    Sixel,
    /// 24-bit `TrueColor` Unicode halfblocks (`▀`/`▄`).
    Halfblocks,
}

impl ImageProtocol {
    /// Human-readable label for `vynl doctor` and UI status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::ITerm2 => "iterm2",
            Self::Sixel => "sixel",
            Self::Halfblocks => "halfblocks",
        }
    }

    /// Best-effort environment heuristic (augments terminal query probing and
    /// safely falls back inside `tmux` until `allow-passthrough` is verified).
    #[must_use]
    pub fn detect_from_env(
        term_program: Option<&str>,
        term: Option<&str>,
        kitty_window_id: Option<&str>,
        in_tmux: bool,
    ) -> Self {
        if in_tmux {
            // Safe default until tmux `allow-passthrough` is verified via query.
            return Self::Halfblocks;
        }
        if kitty_window_id.is_some() {
            return Self::Kitty;
        }
        match term_program.unwrap_or_default() {
            "ghostty" | "WezTerm" | "kitty" => Self::Kitty,
            "iTerm.app" => Self::ITerm2,
            _ => match term.unwrap_or_default() {
                "xterm-kitty" | "xterm-ghostty" => Self::Kitty,
                "foot" | "foot-extra" | "mlterm" => Self::Sixel,
                _ => Self::Halfblocks,
            },
        }
    }

    /// Resolves a user [`ImageProtocolPreference`] using environment heuristics when `Auto`.
    #[must_use]
    pub fn resolve(
        preference: ImageProtocolPreference,
        term_program: Option<&str>,
        term: Option<&str>,
        kitty_window_id: Option<&str>,
        in_tmux: bool,
    ) -> Self {
        match preference {
            ImageProtocolPreference::Auto => {
                Self::detect_from_env(term_program, term, kitty_window_id, in_tmux)
            }
            ImageProtocolPreference::Kitty => Self::Kitty,
            ImageProtocolPreference::ITerm2 => Self::ITerm2,
            ImageProtocolPreference::Sixel => Self::Sixel,
            ImageProtocolPreference::Halfblocks => Self::Halfblocks,
        }
    }

    /// Applies smart protocol scaling to decide whether small thumbnails inside
    /// scrolling track/search lists should be rendered.
    #[must_use]
    pub const fn render_list_thumbnails(self, policy: ListThumbnailPolicy) -> bool {
        match policy {
            ListThumbnailPolicy::Always => true,
            ListThumbnailPolicy::Never => false,
            ListThumbnailPolicy::Auto => matches!(self, Self::Kitty | Self::ITerm2),
        }
    }
}

/// Protocol-specific encoded terminal frame ready to write to a terminal cell region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodedImage {
    /// Chunked Kitty Graphics Protocol APC escape sequence.
    Kitty(String),
    /// `iTerm2` OSC 1337 inline image escape sequence.
    ITerm2(String),
    /// Sixel DCS escape sequence.
    Sixel(String),
    /// 24-bit `TrueColor` Unicode halfblock rows (`▀`), one string per terminal row.
    Halfblocks(Vec<String>),
}

impl EncodedImage {
    /// Returns the protocol used to encode this frame.
    #[must_use]
    pub const fn protocol(&self) -> ImageProtocol {
        match self {
            Self::Kitty(_) => ImageProtocol::Kitty,
            Self::ITerm2(_) => ImageProtocol::ITerm2,
            Self::Sixel(_) => ImageProtocol::Sixel,
            Self::Halfblocks(_) => ImageProtocol::Halfblocks,
        }
    }

    /// Returns the total encoded payload size in bytes.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Kitty(s) | Self::ITerm2(s) | Self::Sixel(s) => s.len(),
            Self::Halfblocks(rows) => rows.iter().map(String::len).sum(),
        }
    }
}

/// Encodes `image` using the Kitty Graphics Protocol (`f=24` raw `RGB24`),
/// chunking the `Base64` payload into 4096-byte APC frames (`\x1b_G...\x1b\\`).
#[must_use]
pub fn encode_kitty(image: &RgbImage, cols: u16, rows: u16) -> String {
    if image.is_empty() || cols == 0 || rows == 0 {
        return String::new();
    }

    let b64 = encode_base64(&image.pixels);
    let bytes = b64.as_bytes();
    let total_chunks = bytes.len().div_ceil(KITTY_CHUNK_SIZE);
    let mut out = String::with_capacity(b64.len() + total_chunks * 48);

    for (idx, chunk) in bytes.chunks(KITTY_CHUNK_SIZE).enumerate() {
        let more = u8::from(idx + 1 < total_chunks);
        let chunk_str = std::str::from_utf8(chunk).unwrap_or_default();
        if idx == 0 {
            let _ = write!(
                out,
                "\x1b_Gf=24,s={},v={},c={cols},r={rows},a=T,m={more};{chunk_str}\x1b\\",
                image.width, image.height
            );
        } else {
            let _ = write!(out, "\x1b_Gm={more};{chunk_str}\x1b\\");
        }
    }

    out
}

/// Encodes `image` using the `iTerm2` OSC 1337 Inline Images protocol with a
/// lossless 24-bit BMP payload.
#[must_use]
pub fn encode_iterm2(image: &RgbImage, cols: u16, rows: u16) -> String {
    if image.is_empty() || cols == 0 || rows == 0 {
        return String::new();
    }

    let bmp = image.encode_bmp();
    if bmp.is_empty() {
        return String::new();
    }
    let b64 = encode_base64(&bmp);
    format!(
        "\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=1:{b64}\x07",
        bmp.len()
    )
}

/// Encodes `image` into a Sixel DCS sequence (`\x1bPq...\x1b\\`) with a quantized
/// palette (up to 216 colors in a 6x6x6 RGB cube) and run-length compression.
#[must_use]
pub fn encode_sixel(image: &RgbImage) -> String {
    const CUBE_SIZE: usize = 6 * 6 * 6;

    if image.is_empty() {
        return String::new();
    }
    let Ok(width) = usize::try_from(image.width) else {
        return String::new();
    };
    let Ok(height) = usize::try_from(image.height) else {
        return String::new();
    };

    // Map each pixel to a 6x6x6 quantized RGB cube index, assigning compact
    // Sixel color register IDs only for colors present in the image.
    let mut cube_to_reg = [None::<u8>; CUBE_SIZE];
    let mut palette: Vec<Rgb> = Vec::new();
    let mut indexed = Vec::with_capacity(width.saturating_mul(height));

    for chunk in image.pixels.as_chunks::<3>().0 {
        let rq = quantize_6(chunk[0]);
        let gq = quantize_6(chunk[1]);
        let bq = quantize_6(chunk[2]);
        let cube_idx = usize::from(rq) * 36 + usize::from(gq) * 6 + usize::from(bq);

        let reg = if let Some(existing) = cube_to_reg[cube_idx] {
            existing
        } else {
            let next_reg = u8::try_from(palette.len()).unwrap_or(215);
            cube_to_reg[cube_idx] = Some(next_reg);
            palette.push(Rgb::new(rq * 51, gq * 51, bq * 51));
            next_reg
        };
        indexed.push(reg);
    }

    let mut out = String::with_capacity(width * height / 2 + palette.len() * 20 + 32);
    let _ = write!(out, "\x1bPq\"1;1;{};{}", image.width, image.height);

    // Emit Sixel palette registers (#idx;2;r_pct;g_pct;b_pct).
    for (reg, color) in palette.iter().enumerate() {
        let r_pct = (u32::from(color.r) * 100 + 127) / 255;
        let g_pct = (u32::from(color.g) * 100 + 127) / 255;
        let b_pct = (u32::from(color.b) * 100 + 127) / 255;
        let _ = write!(out, "#{reg};2;{r_pct};{g_pct};{b_pct}");
    }

    let num_bands = height.div_ceil(6);
    let mut active_regs = vec![false; palette.len()];
    let mut row_buf = vec![0u8; width];

    for band in 0..num_bands {
        let y_start = band * 6;
        let y_end = (y_start + 6).min(height);

        active_regs.fill(false);
        for y in y_start..y_end {
            let row_off = y * width;
            for &reg in &indexed[row_off..row_off + width] {
                active_regs[usize::from(reg)] = true;
            }
        }

        let mut first_in_band = true;
        for (reg_usize, &is_active) in active_regs.iter().enumerate() {
            if !is_active {
                continue;
            }
            let Ok(reg) = u8::try_from(reg_usize) else {
                continue;
            };
            if !first_in_band {
                out.push('$');
            }
            first_in_band = false;

            let _ = write!(out, "#{reg_usize}");
            row_buf.fill(0);
            let mut last_non_zero = 0usize;

            for (bit, y) in (y_start..y_end).enumerate() {
                let bit_mask = 1u8 << bit;
                let row_off = y * width;
                for x in 0..width {
                    if indexed[row_off + x] == reg {
                        row_buf[x] |= bit_mask;
                        if x + 1 > last_non_zero {
                            last_non_zero = x + 1;
                        }
                    }
                }
            }

            append_sixel_rle(&mut out, &row_buf[..last_non_zero]);
        }

        if band + 1 < num_bands {
            out.push('-');
        }
    }

    out.push_str("\x1b\\");
    out
}

/// Encodes `image` as 24-bit `TrueColor` Unicode halfblock rows (`▀`), packing
/// two vertical pixels per terminal character cell (`cols` wide by `rows` tall).
#[must_use]
pub fn encode_halfblocks(image: &RgbImage, cols: u16, rows: u16) -> Vec<String> {
    if image.is_empty() || cols == 0 || rows == 0 {
        return Vec::new();
    }

    let target_w = u32::from(cols);
    let target_h = u32::from(rows).saturating_mul(2);
    let resized = if image.width == target_w && image.height == target_h {
        image.clone()
    } else {
        image.resize_bilinear(target_w, target_h)
    };

    let mut lines = Vec::with_capacity(usize::from(rows));
    for row in 0..u32::from(rows) {
        let top_y = row * 2;
        let bot_y = top_y + 1;
        let mut line = String::with_capacity(usize::from(cols) * 24 + 8);
        let mut prev_pair: Option<(Rgb, Rgb)> = None;

        for col in 0..target_w {
            let top = resized.get_pixel(col, top_y).unwrap_or(Rgb::new(0, 0, 0));
            let bot = resized.get_pixel(col, bot_y).unwrap_or(Rgb::new(0, 0, 0));
            let pair = (top, bot);
            if prev_pair != Some(pair) {
                let _ = write!(
                    line,
                    "\x1b[38;2;{};{};{};48;2;{};{};{}m",
                    top.r, top.g, top.b, bot.r, bot.g, bot.b
                );
                prev_pair = Some(pair);
            }
            line.push('▀');
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }

    lines
}

/// Fits `image` to the target terminal cell box and encodes it for `protocol`.
#[must_use]
pub fn encode_for_protocol(
    image: &RgbImage,
    protocol: ImageProtocol,
    cols: u16,
    rows: u16,
    cell_px_w: u16,
    cell_px_h: u16,
) -> EncodedImage {
    match protocol {
        ImageProtocol::Kitty => {
            let fitted = image.fit_to_cells(cols, rows, cell_px_w, cell_px_h);
            EncodedImage::Kitty(encode_kitty(&fitted, cols, rows))
        }
        ImageProtocol::ITerm2 => {
            let fitted = image.fit_to_cells(cols, rows, cell_px_w, cell_px_h);
            EncodedImage::ITerm2(encode_iterm2(&fitted, cols, rows))
        }
        ImageProtocol::Sixel => {
            let fitted = image.fit_to_cells(cols, rows, cell_px_w, cell_px_h);
            EncodedImage::Sixel(encode_sixel(&fitted))
        }
        ImageProtocol::Halfblocks => EncodedImage::Halfblocks(encode_halfblocks(image, cols, rows)),
    }
}

fn quantize_6(channel: u8) -> u8 {
    u8::try_from((u16::from(channel) * 5 + 127) / 255).unwrap_or(5)
}

fn append_sixel_rle(out: &mut String, masks: &[u8]) {
    let mut idx = 0;
    while idx < masks.len() {
        let mask = masks[idx];
        let mut run = 1usize;
        while idx + run < masks.len() && masks[idx + run] == mask {
            run += 1;
        }
        let ch = char::from(63 + mask);
        if run >= 4 {
            let _ = write!(out, "!{run}{ch}");
        } else {
            for _ in 0..run {
                out.push(ch);
            }
        }
        idx += run;
    }
}

#[cfg(test)]
mod tests {
    use vynl_core::config::ListThumbnailPolicy;
    use vynl_core::theme::Rgb;

    use crate::image::{RgbImage, generate_demo_cover};

    use super::{ImageProtocol, encode_halfblocks, encode_iterm2, encode_kitty, encode_sixel};

    #[test]
    fn smart_protocol_scaling_enables_list_thumbnails_on_fast_protocols_only() {
        assert!(ImageProtocol::Kitty.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(ImageProtocol::ITerm2.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(!ImageProtocol::Sixel.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(!ImageProtocol::Halfblocks.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(ImageProtocol::Halfblocks.render_list_thumbnails(ListThumbnailPolicy::Always));
        assert!(!ImageProtocol::Kitty.render_list_thumbnails(ListThumbnailPolicy::Never));
    }

    #[test]
    fn detects_known_terminals_and_falls_back_in_tmux() {
        assert_eq!(
            ImageProtocol::detect_from_env(Some("ghostty"), None, None, false),
            ImageProtocol::Kitty
        );
        assert_eq!(
            ImageProtocol::detect_from_env(Some("iTerm.app"), None, None, false),
            ImageProtocol::ITerm2
        );
        assert_eq!(
            ImageProtocol::detect_from_env(None, Some("foot"), None, false),
            ImageProtocol::Sixel
        );
        assert_eq!(
            ImageProtocol::detect_from_env(Some("ghostty"), None, Some("1"), true),
            ImageProtocol::Halfblocks
        );
    }

    #[test]
    fn kitty_encoder_chunks_payload_at_4096_bytes() {
        // 48x48 RGB24 = 6912 bytes -> 9216 base64 chars -> 3 chunks (4096, 4096, 1024).
        let img = generate_demo_cover("kitty-chunk-test", 48);
        let seq = encode_kitty(&img, 12, 6);

        assert!(seq.starts_with("\x1b_Gf=24,s=48,v=48,c=12,r=6,a=T,m=1;"));
        assert!(seq.ends_with("\x1b\\"));

        let chunks: Vec<&str> = seq.split("\x1b\\").filter(|s| !s.is_empty()).collect();
        assert_eq!(chunks.len(), 3);
        assert!(chunks[1].starts_with("\x1b_Gm=1;"));
        assert!(chunks[2].starts_with("\x1b_Gm=0;"));

        for chunk in chunks {
            let payload = chunk.split_once(';').map_or("", |(_, p)| p);
            assert!(payload.len() <= 4096);
        }
    }

    #[test]
    fn iterm2_encoder_emits_osc_1337_with_bmp_payload() {
        let img = generate_demo_cover("iterm-test", 16);
        let seq = encode_iterm2(&img, 10, 5);
        assert!(seq.starts_with("\x1b]1337;File=inline=1;size="));
        assert!(seq.contains(";width=10;height=5;preserveAspectRatio=1:"));
        assert!(seq.ends_with('\x07'));
        // Base64 of "BM" starts with "Qk0".
        assert!(seq.contains(":Qk0"));
    }

    #[test]
    fn sixel_encoder_emits_dcs_palette_and_bands() {
        let img = generate_demo_cover("sixel-test", 18);
        let seq = encode_sixel(&img);
        assert!(seq.starts_with("\x1bPq\"1;1;18;18"));
        assert!(seq.contains("#0;2;"));
        // 18 rows = 3 sixel bands -> 2 band newlines ('-').
        assert_eq!(seq.matches('-').count(), 2);
        assert!(seq.ends_with("\x1b\\"));
    }

    #[test]
    fn halfblocks_encoder_packs_two_vertical_pixels_per_cell() {
        let mut img = RgbImage::filled(4, 4, Rgb::new(200, 10, 20));
        for x in 0..4 {
            assert!(img.set_pixel(x, 1, Rgb::new(10, 20, 220)));
            assert!(img.set_pixel(x, 3, Rgb::new(30, 180, 40)));
        }

        let rows = encode_halfblocks(&img, 4, 2);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("\x1b[38;2;200;10;20;48;2;10;20;220m"));
        assert!(rows[1].contains("\x1b[38;2;200;10;20;48;2;30;180;40m"));
        for row in &rows {
            assert_eq!(row.matches('▀').count(), 4);
            assert!(row.ends_with("\x1b[0m"));
        }
    }
}
