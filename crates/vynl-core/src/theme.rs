//! Modular theme definitions, built-in presets, custom TOML loading, and WCAG-AA contrast math.

use crate::config::{parse_toml_string, strip_toml_comment};

/// 24-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
}

impl Rgb {
    /// Constructs a new sRGB color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `#RRGGBB` hex color notation.
    ///
    /// # Errors
    /// Returns an error if `hex` is not a valid 6-digit hex color with leading `#`.
    pub fn from_hex(hex: &str) -> Result<Self, String> {
        let s = hex.trim();
        let bytes = s
            .strip_prefix('#')
            .ok_or_else(|| format!("expected `#RRGGBB`, got `{hex}`"))?;
        if bytes.len() != 6 {
            return Err(format!("expected 6 hex digits in `{hex}`"));
        }
        let r = u8::from_str_radix(&bytes[0..2], 16)
            .map_err(|_| format!("invalid red hex in `{hex}`"))?;
        let g = u8::from_str_radix(&bytes[2..4], 16)
            .map_err(|_| format!("invalid green hex in `{hex}`"))?;
        let b = u8::from_str_radix(&bytes[4..6], 16)
            .map_err(|_| format!("invalid blue hex in `{hex}`"))?;
        Ok(Self { r, g, b })
    }

    /// Computes relative luminance per WCAG 2.1 (`0.0..=1.0`).
    #[must_use]
    pub fn relative_luminance(self) -> f64 {
        let linearize = |channel: u8| -> f64 {
            let s = f64::from(channel) / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };

        0.2126 * linearize(self.r) + 0.7152 * linearize(self.g) + 0.0722 * linearize(self.b)
    }

    /// Computes the WCAG 2.1 contrast ratio between `self` and `other` (`1.0..=21.0`).
    #[must_use]
    pub fn contrast_ratio(self, other: Self) -> f64 {
        let l1 = self.relative_luminance();
        let l2 = other.relative_luminance();
        let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Gently brightens a color toward white by `factor` (`0.0..=1.0`) to rescue
    /// dark cover accents so they satisfy WCAG-AA contrast.
    #[must_use]
    pub fn lighten(self, factor: f64) -> Self {
        let f = factor.clamp(0.0, 1.0);
        let lift = |c: u8| -> u8 {
            let v = f64::from(c) + (255.0 - f64::from(c)) * f;
            // Clamp ensures lossless non-negative conversion.
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let rounded = v.round().clamp(0.0, 255.0) as u8;
            rounded
        };
        Self::new(lift(self.r), lift(self.g), lift(self.b))
    }
}

/// Minimum WCAG-AA contrast ratio for normal UI text and active highlights.
pub const WCAG_AA_MIN_CONTRAST: f64 = 4.5;

/// Semantic color palette for `vynl` (built-in or loaded from `~/.config/vynl/themes/*.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Unique theme identifier.
    pub name: String,
    /// Main application background.
    pub background: Rgb,
    /// Elevated surface (cards, sidebars, player bar).
    pub surface: Rgb,
    /// Primary foreground text.
    pub text_primary: Rgb,
    /// Secondary muted metadata text.
    pub text_muted: Rgb,
    /// Primary accent (active progress bar, focus border, selection indicator).
    pub accent: Rgb,
    /// Subtle border color.
    pub border: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        Self::vynl_dark()
    }
}

impl Theme {
    /// Built-in default dark theme.
    #[must_use]
    pub fn vynl_dark() -> Self {
        Self {
            name: String::from("vynl-dark"),
            background: Rgb::new(18, 18, 18),
            surface: Rgb::new(24, 24, 24),
            text_primary: Rgb::new(245, 245, 245),
            text_muted: Rgb::new(167, 167, 167),
            accent: Rgb::new(29, 185, 84),
            border: Rgb::new(48, 48, 48),
        }
    }

    /// Names of all built-in community presets.
    #[must_use]
    pub const fn builtin_names() -> &'static [&'static str] {
        &[
            "vynl-dark",
            "catppuccin-mocha",
            "tokyo-night",
            "gruvbox",
            "nord",
        ]
    }

    /// Returns a built-in community theme preset by name.
    #[must_use]
    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "vynl-dark" => Some(Self::vynl_dark()),
            "catppuccin-mocha" => Some(Self {
                name: String::from("catppuccin-mocha"),
                background: Rgb::new(30, 30, 46),
                surface: Rgb::new(24, 24, 37),
                text_primary: Rgb::new(205, 214, 244),
                text_muted: Rgb::new(147, 153, 178),
                accent: Rgb::new(166, 227, 161),
                border: Rgb::new(69, 71, 90),
            }),
            "tokyo-night" => Some(Self {
                name: String::from("tokyo-night"),
                background: Rgb::new(26, 27, 38),
                surface: Rgb::new(22, 22, 30),
                text_primary: Rgb::new(192, 202, 245),
                text_muted: Rgb::new(120, 124, 153),
                accent: Rgb::new(122, 162, 247),
                border: Rgb::new(59, 66, 97),
            }),
            "gruvbox" => Some(Self {
                name: String::from("gruvbox"),
                background: Rgb::new(40, 40, 40),
                surface: Rgb::new(29, 32, 33),
                text_primary: Rgb::new(235, 219, 178),
                text_muted: Rgb::new(168, 153, 132),
                accent: Rgb::new(184, 187, 38),
                border: Rgb::new(80, 73, 69),
            }),
            "nord" => Some(Self {
                name: String::from("nord"),
                background: Rgb::new(46, 52, 64),
                surface: Rgb::new(59, 66, 82),
                text_primary: Rgb::new(236, 239, 244),
                text_muted: Rgb::new(216, 222, 233),
                accent: Rgb::new(136, 192, 208),
                border: Rgb::new(76, 86, 106),
            }),
            _ => None,
        }
    }

    /// Parses a custom community theme file (`~/.config/vynl/themes/<name>.toml`).
    ///
    /// # Errors
    /// Returns an error on invalid TOML syntax or malformed `#RRGGBB` hex colors.
    pub fn from_toml(name: &str, input: &str) -> Result<Self, String> {
        let mut theme = Self::vynl_dark();
        name.clone_into(&mut theme.name);

        for (idx, raw_line) in input.lines().enumerate() {
            let line_no = idx + 1;
            let line = strip_toml_comment(raw_line).trim();
            if line.is_empty() || (line.starts_with('[') && line.ends_with(']')) {
                continue;
            }
            let Some((raw_key, raw_val)) = line.split_once('=') else {
                return Err(format!("line {line_no}: expected `key = \"#RRGGBB\"`"));
            };
            let key = raw_key.trim();
            let val = parse_toml_string(raw_val.trim(), line_no)?;
            match key {
                "name" => theme.name = val,
                "background" => theme.background = Rgb::from_hex(&val)?,
                "surface" => theme.surface = Rgb::from_hex(&val)?,
                "text_primary" => theme.text_primary = Rgb::from_hex(&val)?,
                "text_muted" => theme.text_muted = Rgb::from_hex(&val)?,
                "accent" => theme.accent = Rgb::from_hex(&val)?,
                "border" => theme.border = Rgb::from_hex(&val)?,
                _ => {}
            }
        }
        Ok(theme)
    }

    /// Returns a copy of this theme with `accent` overridden only if the candidate
    /// satisfies WCAG-AA contrast against both `background` and `surface`.
    #[must_use]
    pub fn with_adaptive_accent(&self, candidate: Rgb) -> Self {
        let mut next = self.clone();
        if candidate.contrast_ratio(self.background) >= WCAG_AA_MIN_CONTRAST
            && candidate.contrast_ratio(self.surface) >= WCAG_AA_MIN_CONTRAST
        {
            next.accent = candidate;
        }
        next
    }

    /// Picks the highest-contrast candidate from a cover palette, gently lifting
    /// luminance if needed to meet WCAG-AA while preserving hue character.
    #[must_use]
    pub fn with_adaptive_palette(&self, candidates: &[Rgb]) -> Self {
        for &color in candidates {
            if color.contrast_ratio(self.background) >= WCAG_AA_MIN_CONTRAST
                && color.contrast_ratio(self.surface) >= WCAG_AA_MIN_CONTRAST
            {
                return self.with_adaptive_accent(color);
            }
        }
        for &color in candidates {
            let lifted = color.lighten(0.35);
            if lifted.contrast_ratio(self.background) >= WCAG_AA_MIN_CONTRAST
                && lifted.contrast_ratio(self.surface) >= WCAG_AA_MIN_CONTRAST
            {
                return self.with_adaptive_accent(lifted);
            }
        }
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{Rgb, Theme, WCAG_AA_MIN_CONTRAST};

    #[test]
    fn all_builtin_presets_satisfy_wcag_aa_for_primary_text_and_accent() {
        for preset in Theme::builtin_names() {
            let Some(theme) = Theme::builtin(preset) else {
                unreachable!("builtin preset must exist");
            };
            assert!(
                theme.text_primary.contrast_ratio(theme.background) >= WCAG_AA_MIN_CONTRAST,
                "primary text contrast failed for {preset}"
            );
            assert!(
                theme.accent.contrast_ratio(theme.background) >= WCAG_AA_MIN_CONTRAST,
                "accent contrast failed for {preset}"
            );
        }
    }

    #[test]
    fn custom_toml_theme_parses_hex_colors() -> Result<(), String> {
        let toml = r##"
            name = "rose-pine"
            background = "#191724"
            surface = "#1f1d2e"
            text_primary = "#e0def4"
            text_muted = "#908caa"
            accent = "#ebbcba"
            border = "#403d52"
        "##;
        let theme = Theme::from_toml("rose-pine", toml)?;
        assert_eq!(theme.name, "rose-pine");
        assert_eq!(theme.accent, Rgb::new(0xeb, 0xbc, 0xba));
        Ok(())
    }

    #[test]
    fn adaptive_accent_rejects_low_contrast_color() {
        let base = Theme::vynl_dark();
        let too_dark = Rgb::new(30, 30, 30);
        let kept = base.with_adaptive_accent(too_dark);
        assert_eq!(kept.accent, base.accent);

        let bright_coral = Rgb::new(255, 128, 114);
        let updated = base.with_adaptive_accent(bright_coral);
        assert_eq!(updated.accent, bright_coral);
    }
}
