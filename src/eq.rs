use std::{f32::consts::PI, time::Duration};

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

pub struct EqualizerSource<I>
where
    I: Source,
{
    input: I,
    filters: Vec<BandRuntime>,
    sample_index: usize,
}

impl<I> EqualizerSource<I>
where
    I: Source,
{
    pub fn new(input: I, bands: Vec<EqBand>) -> Self {
        let channels = input.channels().max(1);
        let sample_rate = input.sample_rate();
        let filters = bands
            .into_iter()
            .filter(|band| band.frequency_hz > 0.0 && band.q > 0.0 && band.gain_db.abs() > 0.01)
            .map(|band| BandRuntime::new(&band, channels, sample_rate))
            .collect();

        Self {
            input,
            filters,
            sample_index: 0,
        }
    }

    fn reset(&mut self) {
        self.sample_index = 0;
        for filter in &mut self.filters {
            filter.reset();
        }
    }
}

impl<I> Iterator for EqualizerSource<I>
where
    I: Source,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let mut sample = self.input.next()?;
        let channel = self.sample_index % usize::from(self.channels().max(1));

        for filter in &mut self.filters {
            sample = filter.process(channel, sample);
        }

        self.sample_index = self.sample_index.wrapping_add(1);
        Some(sample.clamp(-1.0, 1.0))
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
