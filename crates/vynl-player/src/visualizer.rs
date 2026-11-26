//! Pure-Rust Cooley-Tukey Radix-2 FFT audio spectrum visualizer.
//!
//! Full FFT analysis ([`Complex`], [`radix2_fft`], [`hann_window`], [`SpectrumAnalyzer`])
//! is enabled with the `visualizer` Cargo feature. [`is_enabled`] is always available
//! so callers can inspect compile-time availability under `--no-default-features`.

/// Returns `true` when the crate was compiled with the `visualizer` feature enabled.
#[must_use]
pub const fn is_enabled() -> bool {
    cfg!(feature = "visualizer")
}

#[cfg(feature = "visualizer")]
mod fft_impl {
    use std::f64::consts::PI;
    use std::ops::{Add, Mul, Sub};

    use crate::error::PlayerError;

    /// Double-precision complex number used by the pure-Rust Cooley-Tukey Radix-2 FFT.
    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct Complex {
        /// Real component.
        pub re: f64,
        /// Imaginary component.
        pub im: f64,
    }

    impl Complex {
        /// Creates a new complex number `re + i * im`.
        #[must_use]
        pub const fn new(re: f64, im: f64) -> Self {
            Self { re, im }
        }

        /// Zero complex number (`0 + 0i`).
        pub const ZERO: Self = Self { re: 0.0, im: 0.0 };

        /// Euclidean magnitude `sqrt(re^2 + im^2)`.
        #[must_use]
        pub fn magnitude(self) -> f64 {
            self.re.hypot(self.im)
        }
    }

    impl Add for Complex {
        type Output = Self;

        fn add(self, rhs: Self) -> Self::Output {
            Self {
                re: self.re + rhs.re,
                im: self.im + rhs.im,
            }
        }
    }

    impl Sub for Complex {
        type Output = Self;

        fn sub(self, rhs: Self) -> Self::Output {
            Self {
                re: self.re - rhs.re,
                im: self.im - rhs.im,
            }
        }
    }

    impl Mul for Complex {
        type Output = Self;

        fn mul(self, rhs: Self) -> Self::Output {
            Self {
                re: self.re * rhs.re - self.im * rhs.im,
                im: self.re * rhs.im + self.im * rhs.re,
            }
        }
    }

    /// Generates a symmetric Hann window of length `len`.
    #[must_use]
    pub fn hann_window(len: usize) -> Vec<f64> {
        if len == 0 {
            return Vec::new();
        }
        if len == 1 {
            return vec![1.0];
        }
        #[allow(clippy::cast_precision_loss)]
        let denom = (len - 1) as f64;
        (0..len)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let idx = i as f64;
                0.5 * (1.0 - (2.0 * PI * idx / denom).cos())
            })
            .collect()
    }

    /// Performs an in-place iterative **Cooley-Tukey Radix-2** forward FFT.
    ///
    /// Runs in `O(N log N)` time with zero heap allocations and zero `unsafe` code.
    ///
    /// # Errors
    /// Returns [`PlayerError::InvalidState`] if `buffer.len()` is zero or not a power of two.
    pub fn radix2_fft(buffer: &mut [Complex]) -> Result<(), PlayerError> {
        let count = buffer.len();
        if count == 0 || !count.is_power_of_two() {
            return Err(PlayerError::InvalidState(format!(
                "FFT buffer length must be a non-zero power of two (got {count})"
            )));
        }
        if count == 1 {
            return Ok(());
        }

        // Bit-reversal permutation.
        let mut rev: usize = 0;
        for idx in 1..count {
            let mut bit = count >> 1;
            while (rev & bit) != 0 {
                rev ^= bit;
                bit >>= 1;
            }
            rev ^= bit;
            if idx < rev {
                buffer.swap(idx, rev);
            }
        }

        // Iterative decimation-in-time butterfly stages.
        let mut stage_len = 2;
        while stage_len <= count {
            #[allow(clippy::cast_precision_loss)]
            let angle = -2.0 * PI / (stage_len as f64);
            let twiddle_step = Complex::new(angle.cos(), angle.sin());
            let half = stage_len / 2;

            let mut start = 0;
            while start < count {
                let mut twiddle = Complex::new(1.0, 0.0);
                for offset in 0..half {
                    let even = buffer[start + offset];
                    let odd = buffer[start + offset + half] * twiddle;
                    buffer[start + offset] = even + odd;
                    buffer[start + offset + half] = even - odd;
                    twiddle = twiddle * twiddle_step;
                }
                start += stage_len;
            }
            stage_len <<= 1;
        }

        Ok(())
    }

    /// Real-time spectrum analyzer computing normalized logarithmic frequency bars (`0.0..=1.0`)
    /// from PCM windows using a Hann window, Cooley-Tukey Radix-2 FFT, and smooth bar decay.
    #[derive(Debug, Clone)]
    pub struct SpectrumAnalyzer {
        fft_size: usize,
        band_count: usize,
        sample_rate: u32,
        channels: u16,
        smoothing: f32,
        min_freq_hz: f64,
        max_freq_hz: f64,
        floor_db: f64,
        window: Vec<f64>,
        ring: Vec<f64>,
        fft_buf: Vec<Complex>,
        bars: Vec<f32>,
    }

    impl SpectrumAnalyzer {
        /// Default FFT window size (1024 samples ~ 23 ms at 44.1 kHz).
        pub const DEFAULT_FFT_SIZE: usize = 1024;

        /// Default number of logarithmic visualizer bars.
        pub const DEFAULT_BAND_COUNT: usize = 32;

        /// Creates a new [`SpectrumAnalyzer`].
        ///
        /// # Errors
        /// Returns [`PlayerError::InvalidState`] when `fft_size < 16` or is not a power of two,
        /// `band_count == 0`, `sample_rate == 0`, or `channels == 0`.
        pub fn new(
            fft_size: usize,
            band_count: usize,
            sample_rate: u32,
            channels: u16,
        ) -> Result<Self, PlayerError> {
            if fft_size < 16 || !fft_size.is_power_of_two() {
                return Err(PlayerError::InvalidState(format!(
                    "fft_size must be a power of two >= 16 (got {fft_size})"
                )));
            }
            if band_count == 0 {
                return Err(PlayerError::InvalidState(String::from(
                    "band_count must be at least 1",
                )));
            }
            if sample_rate == 0 || channels == 0 {
                return Err(PlayerError::InvalidFormat(format!(
                    "invalid analyzer format: {sample_rate} Hz, {channels} ch"
                )));
            }

            let nyquist = f64::from(sample_rate) * 0.5;
            let max_freq_hz = 16_000.0_f64.min(nyquist).max(100.0);
            let min_freq_hz = 40.0_f64.min(max_freq_hz * 0.5);

            Ok(Self {
                fft_size,
                band_count,
                sample_rate,
                channels,
                smoothing: 0.72,
                min_freq_hz,
                max_freq_hz,
                floor_db: -60.0,
                window: hann_window(fft_size),
                ring: vec![0.0; fft_size],
                fft_buf: vec![Complex::ZERO; fft_size],
                bars: vec![0.0; band_count],
            })
        }

        /// Configures bar decay smoothing (`0.0..=0.99`; higher values decay more slowly).
        #[must_use]
        pub fn with_smoothing(mut self, decay: f32) -> Self {
            self.smoothing = if decay.is_finite() {
                decay.clamp(0.0, 0.99)
            } else {
                0.72
            };
            self
        }

        /// Configures the logarithmic frequency range mapped across bands (clamped to Nyquist).
        #[must_use]
        pub fn with_frequency_range(mut self, min_hz: f64, max_hz: f64) -> Self {
            let nyquist = f64::from(self.sample_rate) * 0.5;
            let upper = if max_hz.is_finite() {
                max_hz.clamp(20.0, nyquist)
            } else {
                nyquist
            };
            let lower = if min_hz.is_finite() {
                min_hz.clamp(1.0, (upper * 0.9).max(1.0))
            } else {
                40.0_f64.min(upper * 0.5)
            };
            self.min_freq_hz = lower;
            self.max_freq_hz = upper;
            self
        }

        /// Returns the FFT window size in samples.
        #[must_use]
        pub const fn fft_size(&self) -> usize {
            self.fft_size
        }

        /// Returns the number of output frequency bands.
        #[must_use]
        pub const fn band_count(&self) -> usize {
            self.band_count
        }

        /// Returns the configured sampling rate in Hz.
        #[must_use]
        pub const fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        /// Returns the configured channel count.
        #[must_use]
        pub const fn channels(&self) -> u16 {
            self.channels
        }

        /// Returns the latest normalized band heights in `0.0..=1.0`.
        #[must_use]
        pub fn bars(&self) -> &[f32] {
            &self.bars
        }

        /// Resets internal sample history, FFT buffer, and visualizer bars to zero.
        pub fn reset(&mut self) {
            self.ring.fill(0.0);
            self.fft_buf.fill(Complex::ZERO);
            self.bars.fill(0.0);
        }

        /// Returns the geometric center frequency in Hz for `band_index`, if in range.
        #[must_use]
        pub fn band_center_hz(&self, band_index: usize) -> Option<f64> {
            if band_index >= self.band_count {
                return None;
            }
            let (low, high) = self.band_edges_hz(band_index);
            Some((low * high).sqrt())
        }

        /// Pushes interleaved signed 16-bit PCM frames, downmixes to mono, runs the FFT,
        /// and returns the updated normalized frequency bars (`0.0..=1.0`).
        pub fn push_interleaved_pcm(&mut self, samples: &[i16]) -> &[f32] {
            let ch = usize::from(self.channels);
            if ch == 0 || samples.is_empty() {
                return &self.bars;
            }
            let inv_scale = 1.0 / (32_768.0 * f64::from(self.channels));
            let mono: Vec<f64> = samples
                .chunks_exact(ch)
                .map(|frame| {
                    let sum: f64 = frame.iter().map(|&s| f64::from(s)).sum();
                    sum * inv_scale
                })
                .collect();
            self.analyze_mono(&mono)
        }

        /// Pushes mono `f32` samples in `-1.0..=1.0`, runs the FFT, and returns updated bars.
        pub fn push_mono_f32(&mut self, samples: &[f32]) -> &[f32] {
            let mono: Vec<f64> = samples
                .iter()
                .map(|&s| if s.is_finite() { f64::from(s) } else { 0.0 })
                .collect();
            self.analyze_mono(&mono)
        }

        /// Pushes mono `f64` samples into the rolling window, applies the Hann window,
        /// executes [`radix2_fft`], and updates the smoothed frequency bars.
        pub fn analyze_mono(&mut self, samples: &[f64]) -> &[f32] {
            if !samples.is_empty() {
                if samples.len() >= self.fft_size {
                    let start = samples.len() - self.fft_size;
                    self.ring.copy_from_slice(&samples[start..]);
                } else {
                    let keep = self.fft_size - samples.len();
                    self.ring.copy_within(samples.len().., 0);
                    self.ring[keep..].copy_from_slice(samples);
                }
            }

            for i in 0..self.fft_size {
                self.fft_buf[i] = Complex::new(self.ring[i] * self.window[i], 0.0);
            }

            if radix2_fft(&mut self.fft_buf).is_err() {
                return &self.bars;
            }

            self.update_bars_from_spectrum();
            &self.bars
        }

        /// Returns the dominant frequency in Hz from the most recent FFT frame (`0.0` if silent).
        #[must_use]
        pub fn peak_frequency_hz(&self) -> f64 {
            let half = self.fft_size / 2;
            let mut best_bin = 0usize;
            let mut best_mag = 0.0_f64;

            for (bin, value) in self.fft_buf.iter().enumerate().take(half).skip(1) {
                let mag = value.magnitude();
                if mag > best_mag {
                    best_mag = mag;
                    best_bin = bin;
                }
            }

            if best_bin == 0 || best_mag <= 1e-9 {
                return 0.0;
            }

            #[allow(clippy::cast_precision_loss)]
            let freq = (best_bin as f64) * f64::from(self.sample_rate) / (self.fft_size as f64);
            freq
        }

        fn band_edges_hz(&self, band_index: usize) -> (f64, f64) {
            let ratio = self.max_freq_hz / self.min_freq_hz;
            #[allow(clippy::cast_precision_loss)]
            let total = self.band_count as f64;
            #[allow(clippy::cast_precision_loss)]
            let b0 = band_index as f64 / total;
            #[allow(clippy::cast_precision_loss)]
            let b1 = (band_index + 1) as f64 / total;
            (
                self.min_freq_hz * ratio.powf(b0),
                self.min_freq_hz * ratio.powf(b1),
            )
        }

        fn update_bars_from_spectrum(&mut self) {
            let half = self.fft_size / 2;
            #[allow(clippy::cast_precision_loss)]
            let bin_hz = f64::from(self.sample_rate) / (self.fft_size as f64);
            // Coherent gain of Hann window is 0.5, single-sided FFT factor is 2.0/N -> 4.0/N.
            #[allow(clippy::cast_precision_loss)]
            let norm = 4.0 / (self.fft_size as f64);

            for band in 0..self.band_count {
                let (low_hz, high_hz) = self.band_edges_hz(band);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let start_bin = ((low_hz / bin_hz).floor() as usize).clamp(1, half - 1);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let end_bin = ((high_hz / bin_hz).ceil() as usize).clamp(start_bin + 1, half);

                let mut peak_amp = 0.0_f64;
                for value in &self.fft_buf[start_bin..end_bin] {
                    let amp = value.magnitude() * norm;
                    if amp > peak_amp {
                        peak_amp = amp;
                    }
                }

                let target = if peak_amp <= 1e-9 {
                    0.0_f32
                } else {
                    let db = 20.0 * peak_amp.log10();
                    let normalized = ((db - self.floor_db) / (-self.floor_db)).clamp(0.0, 1.0);
                    #[allow(clippy::cast_possible_truncation)]
                    let out = normalized as f32;
                    out
                };

                let prev = self.bars[band];
                self.bars[band] = if target >= prev {
                    target
                } else {
                    let decayed = prev * self.smoothing;
                    if decayed < 1e-4 {
                        target
                    } else {
                        decayed.max(target)
                    }
                };
            }
        }
    }
}

#[cfg(feature = "visualizer")]
pub use fft_impl::{Complex, SpectrumAnalyzer, hann_window, radix2_fft};

#[cfg(test)]
mod tests {
    use super::is_enabled;

    #[test]
    fn reports_visualizer_feature_flag_consistently() {
        assert_eq!(is_enabled(), cfg!(feature = "visualizer"));
    }

    #[cfg(feature = "visualizer")]
    #[test]
    fn radix2_fft_validates_length_and_matches_known_dft() -> Result<(), String> {
        use super::{Complex, radix2_fft};

        let mut bad = vec![Complex::ZERO; 6];
        assert!(radix2_fft(&mut bad).is_err());

        // Impulse [1, 0, 0, 0] has flat DFT [1, 1, 1, 1].
        let mut impulse = [
            Complex::new(1.0, 0.0),
            Complex::ZERO,
            Complex::ZERO,
            Complex::ZERO,
        ];
        radix2_fft(&mut impulse).map_err(|e| e.to_string())?;
        for bin in impulse {
            assert!((bin.re - 1.0).abs() < 1e-9);
            assert!(bin.im.abs() < 1e-9);
        }

        // Constant signal [1, 1, 1, 1] concentrates energy at DC bin 0 = 4.
        let mut dc = [Complex::new(1.0, 0.0); 4];
        radix2_fft(&mut dc).map_err(|e| e.to_string())?;
        assert!((dc[0].re - 4.0).abs() < 1e-9);
        for bin in &dc[1..] {
            assert!(bin.magnitude() < 1e-9);
        }
        Ok(())
    }

    #[cfg(feature = "visualizer")]
    #[test]
    fn spectrum_analyzer_detects_pure_sine_wave_peak_and_decays_smoothly() -> Result<(), String> {
        use std::f64::consts::PI;

        use super::SpectrumAnalyzer;

        let sample_rate = 44_100_u32;
        let fft_size = 1024_usize;
        let band_count = 32_usize;
        let target_hz = 1_000.0_f64;

        let mut analyzer = SpectrumAnalyzer::new(fft_size, band_count, sample_rate, 2)
            .map_err(|e| e.to_string())?
            .with_smoothing(0.75);

        // Synthesize a stereo 1 kHz sine wave at 90% full-scale signed 16-bit PCM.
        let mut stereo_pcm = Vec::with_capacity(fft_size * 2);
        for i in 0..fft_size {
            #[allow(clippy::cast_precision_loss)]
            let t = (i as f64) / f64::from(sample_rate);
            let sample_f = (2.0 * PI * target_hz * t).sin() * 29_000.0;
            #[allow(clippy::cast_possible_truncation)]
            let sample = sample_f.round() as i16;
            stereo_pcm.push(sample);
            stereo_pcm.push(sample);
        }

        let bars = analyzer.push_interleaved_pcm(&stereo_pcm).to_vec();
        assert_eq!(bars.len(), band_count);
        for &bar in &bars {
            assert!((0.0..=1.0).contains(&bar));
        }

        // Peak frequency must fall within 1 FFT bin (~43.1 Hz) of 1000 Hz.
        let bin_width_hz = f64::from(sample_rate) / 1024.0;
        let detected_hz = analyzer.peak_frequency_hz();
        assert!(
            (detected_hz - target_hz).abs() <= bin_width_hz,
            "expected peak near {target_hz} Hz, got {detected_hz} Hz"
        );

        // Find the peak bar and confirm its band center is near 1000 Hz and amplitude is high.
        let mut peak_band = 0usize;
        let mut peak_val = 0.0_f32;
        for (idx, &val) in bars.iter().enumerate() {
            if val > peak_val {
                peak_val = val;
                peak_band = idx;
            }
        }
        assert!(peak_val > 0.85);
        let Some(center_hz) = analyzer.band_center_hz(peak_band) else {
            return Err(String::from("missing band center frequency"));
        };
        assert!(
            center_hz > 750.0 && center_hz < 1_350.0,
            "peak band {peak_band} center {center_hz} Hz not near {target_hz} Hz"
        );

        // Distant low band should remain significantly quieter than the 1 kHz peak.
        assert!(bars[0] < peak_val * 0.5);

        // Feeding silence must trigger smooth exponential decay rather than instant zeroing.
        let silence = vec![0_i16; fft_size * 2];
        let decayed_1 = analyzer.push_interleaved_pcm(&silence)[peak_band];
        let decayed_2 = analyzer.push_interleaved_pcm(&silence)[peak_band];
        assert!(decayed_1 < peak_val);
        assert!(decayed_1 > 0.1);
        assert!(decayed_2 < decayed_1);

        analyzer.reset();
        assert_eq!(analyzer.bars(), &vec![0.0_f32; band_count]);
        assert!((analyzer.peak_frequency_hz() - 0.0).abs() < 1e-9);
        Ok(())
    }

    #[cfg(feature = "visualizer")]
    #[test]
    fn spectrum_analyzer_detects_440hz_concert_a() -> Result<(), String> {
        use std::f64::consts::PI;

        use super::SpectrumAnalyzer;

        let sample_rate = 44_100_u32;
        let fft_size = 2048_usize;
        let mut analyzer =
            SpectrumAnalyzer::new(fft_size, 16, sample_rate, 1).map_err(|e| e.to_string())?;

        let mono: Vec<f32> = (0..fft_size)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = (i as f64) / f64::from(sample_rate);
                #[allow(clippy::cast_possible_truncation)]
                let v = (2.0 * PI * 440.0 * t).sin() as f32;
                v
            })
            .collect();

        analyzer.push_mono_f32(&mono);
        let detected = analyzer.peak_frequency_hz();
        let bin_width = f64::from(sample_rate) / 2048.0;
        assert!(
            (detected - 440.0).abs() <= bin_width,
            "expected ~440 Hz, got {detected} Hz"
        );
        Ok(())
    }
}
