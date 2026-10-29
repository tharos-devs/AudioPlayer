//! Égaliseur 7 bandes : filtres biquad de l'« Audio EQ Cookbook » (R. Bristow-Johnson),
//! en plateau grave pour la première bande, plateau aigu pour la dernière, en cloche
//! entre les deux, comme la v0.1. Les coefficients sont calculés pour la fréquence
//! d'échantillonnage réelle du fichier (la v0.1 supposait toujours 44,1 kHz).

use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, Sample, SampleRate, Source};

pub const FREQUENCIES: [f64; 7] = [60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0];
const Q: f64 = 1.414;

/// Réglages partagés entre l'interface et les morceaux en cours de décodage.
#[derive(Default)]
pub struct Settings {
    state: RwLock<(bool, [f32; 7])>,
    // Incrémenté à chaque changement : les sources recalculent leurs filtres.
    generation: AtomicU64,
}

impl Settings {
    pub fn set(&self, enabled: bool, gains: [f32; 7]) {
        *self.state.write().unwrap() = (enabled, gains);
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn get(&self) -> (bool, [f32; 7]) {
        *self.state.read().unwrap()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    fn peaking(rate: f64, frequency: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * frequency / rate;
        let alpha = w0.sin() / (2.0 * Q);
        let cos = w0.cos();
        Self::normalized(1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
    }

    fn shelf(rate: f64, frequency: f64, gain_db: f64, low: bool) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * frequency / rate;
        let cos = w0.cos();
        let alpha = w0.sin() / (2.0 * Q);
        let root = 2.0 * a.sqrt() * alpha;
        let s = if low { 1.0 } else { -1.0 };
        let b0 = a * ((a + 1.0) - s * (a - 1.0) * cos + root);
        let b1 = s * 2.0 * a * ((a - 1.0) - s * (a + 1.0) * cos);
        let b2 = a * ((a + 1.0) - s * (a - 1.0) * cos - root);
        let a0 = (a + 1.0) + s * (a - 1.0) * cos + root;
        let a1 = -s * 2.0 * ((a - 1.0) + s * (a + 1.0) * cos);
        let a2 = (a + 1.0) + s * (a - 1.0) * cos - root;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn for_band(rate: f64, band: usize, gain_db: f64) -> Self {
        match band {
            0 => Self::shelf(rate, FREQUENCIES[band], gain_db, true),
            b if b == FREQUENCIES.len() - 1 => Self::shelf(rate, FREQUENCIES[band], gain_db, false),
            _ => Self::peaking(rate, FREQUENCIES[band], gain_db),
        }
    }

    // Forme directe I : état [x1, x2, y1, y2] par canal.
    fn process(&self, x: f64, state: &mut [f64; 4]) -> f64 {
        let y = self.b0 * x + self.b1 * state[0] + self.b2 * state[1] - self.a1 * state[2] - self.a2 * state[3];
        *state = [x, state[0], y, state[2]];
        y
    }
}

/// Applique l'égaliseur à une source ; sans effet (ni coût) quand il est coupé ou à plat.
pub struct EqualizedSource<S: Source> {
    inner: S,
    settings: Arc<Settings>,
    generation: u64,
    filters: Vec<Biquad>,
    // Un état par canal et par bande.
    states: Vec<[f64; 4]>,
    channels: ChannelCount,
    rate: SampleRate,
    channel: usize,
    // Échantillons restants avant de revérifier canaux, fréquence et réglages.
    until_check: usize,
}

impl<S: Source> EqualizedSource<S> {
    pub fn new(inner: S, settings: Arc<Settings>) -> Self {
        let (channels, rate) = (inner.channels(), inner.sample_rate());
        let mut source = EqualizedSource {
            inner,
            settings,
            generation: u64::MAX,
            filters: Vec::new(),
            states: Vec::new(),
            channels,
            rate,
            channel: 0,
            until_check: 0,
        };
        source.refresh(true);
        source
    }

    fn refresh(&mut self, force: bool) {
        let (channels, rate) = (self.inner.channels(), self.inner.sample_rate());
        let generation = self.settings.generation();
        let format_changed = channels != self.channels || rate != self.rate;
        if !force && !format_changed && generation == self.generation {
            return;
        }
        let (enabled, gains) = self.settings.get();
        let rate_hz = rate.get() as f64;
        self.filters = if enabled {
            (0..FREQUENCIES.len())
                .filter(|&band| gains[band] != 0.0)
                .map(|band| Biquad::for_band(rate_hz, band, gains[band] as f64))
                .collect()
        } else {
            Vec::new()
        };
        if format_changed || self.states.len() != self.filters.len() * channels.get() as usize {
            self.states = vec![[0.0; 4]; self.filters.len() * channels.get() as usize];
            self.channel = 0;
        }
        (self.channels, self.rate, self.generation) = (channels, rate, generation);
    }
}

impl<S: Source> Iterator for EqualizedSource<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        if self.until_check == 0 {
            self.refresh(false);
            // Au plus ~10 ms entre deux vérifications, et jamais au milieu d'une trame.
            let frame = self.channels.get() as usize;
            let span = self.inner.current_span_len().unwrap_or(usize::MAX);
            self.until_check = span.min(self.rate.get() as usize / 100 * frame).max(frame);
        }
        self.until_check -= 1;

        let sample = self.inner.next()?;
        if self.filters.is_empty() {
            return Some(sample);
        }
        let bands = self.filters.len();
        let base = self.channel * bands;
        let mut value = sample as f64;
        for (filter, state) in self.filters.iter().zip(&mut self.states[base..base + bands]) {
            value = filter.process(value, state);
        }
        self.channel = (self.channel + 1) % self.channels.get() as usize;
        // Les gains positifs peuvent dépasser la pleine échelle : on écrête plutôt que de saturer.
        Some(value.clamp(-1.0, 1.0) as Sample)
    }
}

impl<S: Source> Source for EqualizedSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(position)?;
        // Le filtre ne doit pas « traîner » l'audio d'avant le saut.
        self.states.iter_mut().for_each(|state| *state = [0.0; 4]);
        self.channel = 0;
        self.until_check = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn sine(frequency: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let count = (rate as f64 * seconds) as usize;
        (0..count).map(|i| (0.25 * (2.0 * PI * frequency * i as f64 / rate as f64).sin()) as f32).collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        let tail = &samples[samples.len() / 2..];
        (tail.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt()
    }

    fn run(settings: &Arc<Settings>, frequency: f64, rate: u32) -> f64 {
        let input = sine(frequency, rate, 0.5);
        let source = SamplesBuffer::new(ChannelCount::new(1).unwrap(), SampleRate::new(rate).unwrap(), input.clone());
        let output: Vec<f32> = EqualizedSource::new(source, settings.clone()).collect();
        assert_eq!(output.len(), input.len());
        20.0 * (rms(&output) / rms(&input)).log10()
    }

    #[test]
    fn disabled_or_flat_is_transparent() {
        let settings = Arc::new(Settings::default());
        assert!(run(&settings, 1000.0, 44_100).abs() < 1e-9);
        settings.set(true, [0.0; 7]);
        assert!(run(&settings, 1000.0, 44_100).abs() < 1e-9);
    }

    // La bande 1 kHz à +6 dB amplifie ~6 dB à 1 kHz, quelle que soit la fréquence d'échantillonnage.
    #[test]
    fn peaking_band_hits_its_gain_at_any_sample_rate() {
        let settings = Arc::new(Settings::default());
        settings.set(true, [0.0, 0.0, 0.0, 0.0, 6.0, 0.0, 0.0]);
        for rate in [44_100, 48_000, 96_000] {
            let gain = run(&settings, 1000.0, rate);
            assert!((gain - 6.0).abs() < 0.3, "{rate} Hz: {gain:.2} dB");
            // Loin de la bande (60 Hz) : presque rien.
            assert!(run(&settings, 60.0, rate).abs() < 0.5);
        }
    }

    #[test]
    fn shelves_lift_their_side() {
        let settings = Arc::new(Settings::default());
        settings.set(true, [-6.0, 0.0, 0.0, 0.0, 0.0, 0.0, 6.0]);
        assert!(run(&settings, 30.0, 48_000) < -4.5);
        assert!(run(&settings, 12_000.0, 48_000) > 4.5);
        assert!(run(&settings, 1000.0, 48_000).abs() < 1.0);
    }
}
