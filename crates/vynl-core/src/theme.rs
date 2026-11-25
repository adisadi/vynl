//! Modular theme definitions, built-in presets, and WCAG-AA contrast utilities.

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
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Computes relative luminance per WCAG 2.1 (`0.0..=1.0`).
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
    pub fn contrast_ratio(self, other: Self) -> f64 {
        let l1 = self.relative_luminance();
        let l2 = other.relative_luminance();
        let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (lighter + 0.05) / (darker + 0.05)
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

impl Theme {
    /// Returns a built-in community theme preset by name.
    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "vynl-dark" => Some(Self {
                name: String::from("vynl-dark"),
                background: Rgb::new(18, 18, 18),
                surface: Rgb::new(24, 24, 24),
                text_primary: Rgb::new(245, 245, 245),
                text_muted: Rgb::new(167, 167, 167),
                accent: Rgb::new(29, 185, 84),
                border: Rgb::new(48, 48, 48),
            }),
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
}

#[cfg(test)]
mod tests {
    use super::{Rgb, Theme, WCAG_AA_MIN_CONTRAST};

    #[test]
    fn all_builtin_presets_satisfy_wcag_aa_for_primary_text_and_accent() {
        for preset in [
            "vynl-dark",
            "catppuccin-mocha",
            "tokyo-night",
            "gruvbox",
            "nord",
        ] {
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
    fn adaptive_accent_rejects_low_contrast_color() {
        let Some(base) = Theme::builtin("vynl-dark") else {
            unreachable!("vynl-dark must exist");
        };
        let too_dark = Rgb::new(30, 30, 30);
        let kept = base.with_adaptive_accent(too_dark);
        assert_eq!(kept.accent, base.accent);

        let bright_coral = Rgb::new(255, 128, 114);
        let updated = base.with_adaptive_accent(bright_coral);
        assert_eq!(updated.accent, bright_coral);
    }
}
