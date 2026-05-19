use std::{
    collections::VecDeque,
    f32::consts::PI,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use rodio::{ChannelCount, SampleRate, Source};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl EqBand {
    pub fn new(frequency_hz: f32, gain_db: f32, q: f32) -> Self {
        Self {
            frequency_hz,
            gain_db,
            q,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqProfile {
    pub name: String,
    pub bands: Vec<EqBand>,
}

#[derive(Clone)]
pub struct EqControl {
    settings: Arc<RwLock<EqRuntimeSettings>>,
}

impl EqControl {
    pub fn new(enabled: bool, bands: Vec<EqBand>) -> Self {
        Self {
            settings: Arc::new(RwLock::new(EqRuntimeSettings {
                enabled,
                bands,
                version: 0,
            })),
        }
    }

    pub fn update(&self, enabled: bool, bands: Vec<EqBand>) {
        let Ok(mut settings) = self.settings.write() else {
            return;
        };

        if settings.enabled != enabled || settings.bands != bands {
            settings.enabled = enabled;
            settings.bands = bands;
            settings.version = settings.version.wrapping_add(1);
        }
    }

    fn snapshot(&self) -> Option<EqRuntimeSettings> {
        self.settings.read().ok().map(|settings| settings.clone())
    }
}

#[derive(Clone)]
struct EqRuntimeSettings {
    enabled: bool,
    bands: Vec<EqBand>,
    version: u64,
}

#[derive(Clone)]
pub struct AudioAnalyzer {
    state: Arc<Mutex<AudioAnalyzerState>>,
}

impl AudioAnalyzer {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(AudioAnalyzerState {
                samples: VecDeque::with_capacity(8192),
                sample_rate: 44_100,
            })),
        }
    }

    pub fn push_samples(&self, samples: &[f32], sample_rate: SampleRate) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        state.sample_rate = sample_rate;
        for sample in samples {
            if state.samples.len() == 8192 {
                state.samples.pop_front();
            }
            state.samples.push_back(*sample);
        }
    }

    pub fn snapshot(&self) -> AnalyzerSnapshot {
        let Ok(state) = self.state.lock() else {
            return AnalyzerSnapshot {
                samples: Vec::new(),
                sample_rate: 44_100,
            };
        };

        AnalyzerSnapshot {
            samples: state.samples.iter().copied().collect(),
            sample_rate: state.sample_rate,
        }
    }
}

struct AudioAnalyzerState {
    samples: VecDeque<f32>,
    sample_rate: SampleRate,
}

pub struct AnalyzerSnapshot {
    pub samples: Vec<f32>,
    pub sample_rate: SampleRate,
}

pub struct EqualizerSource<I>
where
    I: Source,
{
    input: I,
    control: EqControl,
    analyzer: AudioAnalyzer,
    settings_version: u64,
    filters: Vec<BandRuntime>,
    sample_index: usize,
    analyzer_channel_sum: f32,
    analyzer_frames: Vec<f32>,
}

impl<I> EqualizerSource<I>
where
    I: Source,
{
    pub fn new(input: I, control: EqControl, analyzer: AudioAnalyzer) -> Self {
        let channels = input.channels().max(1);
        let sample_rate = input.sample_rate();
        let settings = control.snapshot();
        let settings_version = settings
            .as_ref()
            .map(|settings| settings.version)
            .unwrap_or(0);
        let filters = settings
            .as_ref()
            .map(|settings| build_filters(settings, channels, sample_rate))
            .unwrap_or_default();

        Self {
            input,
            control,
            analyzer,
            settings_version,
            filters,
            sample_index: 0,
            analyzer_channel_sum: 0.0,
            analyzer_frames: Vec::with_capacity(512),
        }
    }

    fn refresh_filters(&mut self) {
        let Some(settings) = self.control.snapshot() else {
            return;
        };

        if settings.version == self.settings_version {
            return;
        }

        self.filters = build_filters(&settings, self.channels().max(1), self.sample_rate());
        self.settings_version = settings.version;
        self.reset();
    }

    fn reset(&mut self) {
        self.sample_index = 0;
        for filter in &mut self.filters {
            filter.reset();
        }
        self.analyzer_channel_sum = 0.0;
        self.analyzer_frames.clear();
    }

    fn analyze_sample(&mut self, channel: usize, sample: f32) {
        let channels = usize::from(self.channels().max(1));
        self.analyzer_channel_sum += sample;

        if channel + 1 == channels {
            self.analyzer_frames
                .push((self.analyzer_channel_sum / channels as f32).clamp(-1.0, 1.0));
            self.analyzer_channel_sum = 0.0;
        }

        if self.analyzer_frames.len() >= 512 {
            self.analyzer
                .push_samples(&self.analyzer_frames, self.sample_rate());
            self.analyzer_frames.clear();
        }
    }
}

impl<I> Iterator for EqualizerSource<I>
where
    I: Source,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.sample_index % 2048 == 0 {
            self.refresh_filters();
        }

        let mut sample = self.input.next()?;
        let channel = self.sample_index % usize::from(self.channels().max(1));

        for filter in &mut self.filters {
            sample = filter.process(channel, sample);
        }

        sample = sample.clamp(-1.0, 1.0);
        self.analyze_sample(channel, sample);
        self.sample_index = self.sample_index.wrapping_add(1);
        Some(sample)
    }
}

impl<I> Source for EqualizerSource<I>
where
    I: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.input.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.input.try_seek(pos)?;
        self.reset();
        Ok(())
    }
}

fn build_filters(
    settings: &EqRuntimeSettings,
    channels: ChannelCount,
    sample_rate: SampleRate,
) -> Vec<BandRuntime> {
    if !settings.enabled {
        return Vec::new();
    }

    settings
        .bands
        .iter()
        .filter(|band| band.frequency_hz > 0.0 && band.q > 0.0 && band.gain_db.abs() > 0.01)
        .map(|band| BandRuntime::new(band, channels, sample_rate))
        .collect()
}

struct BandRuntime {
    coefficients: BiquadCoefficients,
    states: Vec<BiquadState>,
}

impl BandRuntime {
    fn new(band: &EqBand, channels: ChannelCount, sample_rate: SampleRate) -> Self {
        let coefficients = BiquadCoefficients::peaking(
            band.frequency_hz,
            band.gain_db,
            band.q,
            sample_rate as f32,
        );
        let states = vec![BiquadState::default(); usize::from(channels.max(1))];

        Self {
            coefficients,
            states,
        }
    }

    fn process(&mut self, channel: usize, sample: f32) -> f32 {
        let state_index = channel.min(self.states.len().saturating_sub(1));
        self.states[state_index].process(sample, self.coefficients)
    }

    fn reset(&mut self) {
        for state in &mut self.states {
            *state = BiquadState::default();
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BiquadCoefficients {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl BiquadCoefficients {
    fn peaking(frequency_hz: f32, gain_db: f32, q: f32, sample_rate: f32) -> Self {
        let nyquist = sample_rate / 2.0;
        let frequency = frequency_hz.clamp(10.0, nyquist - 10.0);
        let q = q.clamp(0.1, 10.0);
        let a = 10.0_f32.powf(gain_db.clamp(-24.0, 24.0) / 40.0);
        let omega = 2.0 * PI * frequency / sample_rate;
        let alpha = omega.sin() / (2.0 * q);
        let cos_omega = omega.cos();

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_omega;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_omega;
        let a2 = 1.0 - alpha / a;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BiquadState {
    fn process(&mut self, input: f32, coefficients: BiquadCoefficients) -> f32 {
        let output =
            coefficients.b0 * input + coefficients.b1 * self.x1 + coefficients.b2 * self.x2
                - coefficients.a1 * self.y1
                - coefficients.a2 * self.y2;

        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;

        output
    }
}
