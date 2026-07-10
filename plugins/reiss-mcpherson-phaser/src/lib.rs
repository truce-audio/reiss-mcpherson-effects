//! Phaser - cascade of first-order allpass filters whose centre
//! frequency is swept by an LFO.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, toggle, widgets};

use PhaserParamsParamId as P;

const MAX_FILTERS_PER_CHANNEL: usize = 10;
const UPDATE_INTERVAL: u32 = 32;
const MAX_BLOCK: usize = 512;

#[derive(ParamEnum)]
pub enum Waveform {
    Sine,
    Triangle,
    Square,
    Sawtooth,
}

#[derive(ParamEnum)]
pub enum NumFilters {
    #[name = "2"]
    Two,
    #[name = "4"]
    Four,
    #[name = "6"]
    Six,
    #[name = "8"]
    Eight,
    #[name = "10"]
    Ten,
}

impl NumFilters {
    fn count(self) -> usize {
        match self {
            NumFilters::Two => 2,
            NumFilters::Four => 4,
            NumFilters::Six => 6,
            NumFilters::Eight => 8,
            NumFilters::Ten => 10,
        }
    }
}

#[derive(Params)]
pub struct PhaserParams {
    #[param(
        name = "Depth",
        range = "linear(0.0, 1.0)",
        default = 1.0,
        smooth = "exp(5)"
    )]
    pub depth: FloatParam,

    #[param(
        name = "Feedback",
        range = "linear(0.0, 0.9)",
        default = 0.7,
        smooth = "exp(5)"
    )]
    pub feedback: FloatParam,

    #[param(name = "Stages", default = 1)]
    pub stages: EnumParam<NumFilters>,

    #[param(
        name = "Min Freq",
        range = "linear(50.0, 1000.0)",
        default = 80.0,
        unit = "Hz",
        smooth = "exp(5)"
    )]
    pub min_freq: FloatParam,

    #[param(
        name = "Sweep Width",
        short_name = "Sweep",
        range = "linear(50.0, 3000.0)",
        default = 1000.0,
        unit = "Hz",
        smooth = "exp(5)"
    )]
    pub sweep_width: FloatParam,

    #[param(
        name = "LFO Rate",
        short_name = "Rate",
        range = "linear(0.0, 2.0)",
        default = 0.05,
        unit = "Hz",
        smooth = "exp(5)"
    )]
    pub lfo_rate: FloatParam,

    #[param(name = "Waveform", default = 0)]
    pub waveform: EnumParam<Waveform>,

    #[param(name = "Stereo", default = 1)]
    pub stereo: BoolParam,
}

#[derive(Default, Clone, Copy)]
struct Allpass {
    b0: f32,
    b1: f32,
    a1: f32,
    x1: f32,
    y1: f32,
}

impl Allpass {
    fn update(&mut self, discrete_freq: f32) {
        let wc = discrete_freq.min(std::f32::consts::PI * 0.99);
        let t = (wc * 0.5).tan();
        let a0 = t + 1.0;
        let inv = 1.0 / a0;
        self.b0 = (t - 1.0) * inv;
        self.b1 = 1.0; // (t + 1) / a0
        self.a1 = (t - 1.0) * inv;
    }
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 - self.a1 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

fn lfo(phase: f32, waveform: Waveform) -> f32 {
    match waveform {
        Waveform::Sine => 0.5 + 0.5 * (std::f32::consts::TAU * phase).sin(),
        Waveform::Triangle => {
            if phase < 0.25 {
                0.5 + 2.0 * phase
            } else if phase < 0.75 {
                1.0 - 2.0 * (phase - 0.25)
            } else {
                2.0 * (phase - 0.75)
            }
        }
        Waveform::Square => {
            if phase < 0.5 {
                1.0
            } else {
                0.0
            }
        }
        Waveform::Sawtooth => {
            if phase < 0.5 {
                0.5 + phase
            } else {
                phase - 0.5
            }
        }
    }
}

pub struct Phaser;

pub struct PhaserDsp {
    sample_rate: f32,
    inv_sr: f32,
    filters: [[Allpass; MAX_FILTERS_PER_CHANNEL]; 2],
    feedback_state: [f32; 2],
    lfo_phase: f32,
    sample_counter: u32,
}

impl Default for PhaserDsp {
    fn default() -> Self {
        Self {
            sample_rate: 44_100.0,
            inv_sr: 1.0 / 44_100.0,
            filters: [[Allpass::default(); MAX_FILTERS_PER_CHANNEL]; 2],
            feedback_state: [0.0; 2],
            lfo_phase: 0.0,
            sample_counter: 0,
        }
    }
}

impl PluginLogic for Phaser {
    type Params = PhaserParams;
    type DspState = PhaserDsp;

    fn reset(state: &mut Self::DspState, _params: &Self::Params, config: &AudioConfig) {
        #[allow(clippy::cast_possible_truncation)]
        {
            state.sample_rate = config.sample_rate as f32;
            state.inv_sr = 1.0 / state.sample_rate;
        }
        state.filters = [[Allpass::default(); MAX_FILTERS_PER_CHANNEL]; 2];
        state.feedback_state = [0.0; 2];
        state.lfo_phase = 0.0;
        state.sample_counter = 0;
    }

    #[allow(clippy::needless_range_loop)]
    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let total = buffer.num_samples();
        let waveform = params.waveform.value();
        let stereo = params.stereo.value();
        let num_filters = params.stages.value().count();
        let num_ch = buffer.channels().min(state.filters.len());

        let mut depth = [0.0_f32; MAX_BLOCK];
        let mut feedback = [0.0_f32; MAX_BLOCK];
        let mut min_f = [0.0_f32; MAX_BLOCK];
        let mut sweep = [0.0_f32; MAX_BLOCK];
        let mut rate = [0.0_f32; MAX_BLOCK];

        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(MAX_BLOCK);

            // `read_into` advances the smoother by exactly `n`. See
            // the comment in flanger for why `read_block::<MAX_BLOCK>`
            // is wrong inside a dynamic-length chunk loop.
            params.depth.read_into(&mut depth[..n]);
            params.feedback.read_into(&mut feedback[..n]);
            params.min_freq.read_into(&mut min_f[..n]);
            params.sweep_width.read_into(&mut sweep[..n]);
            params.lfo_rate.read_into(&mut rate[..n]);

            // Schedule coefficient updates - each entry is the
            // discrete frequency to install (or NaN for "no update
            // this sample"). One array per channel because the
            // stereo phase offset diverges the two LFOs.
            let mut sched = [[f32::NAN; MAX_BLOCK]; 2];
            for i in 0..n {
                let update_now = state.sample_counter.is_multiple_of(UPDATE_INTERVAL);
                if update_now {
                    for ch in 0..num_ch.min(2) {
                        let ph = if stereo && ch != 0 {
                            (state.lfo_phase + 0.25).rem_euclid(1.0)
                        } else {
                            state.lfo_phase
                        };
                        let centre = min_f[i] + sweep[i] * lfo(ph, waveform);
                        sched[ch][i] = std::f32::consts::TAU * centre * state.inv_sr;
                    }
                }
                state.sample_counter = state.sample_counter.wrapping_add(1);
                state.lfo_phase += rate[i] * state.inv_sr;
                if state.lfo_phase >= 1.0 {
                    state.lfo_phase -= 1.0;
                }
            }

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                let filters = &mut state.filters[ch][..num_filters];
                let sched_ch = &sched[ch.min(1)];
                let mut fb = state.feedback_state[ch];
                for i in 0..n {
                    let discrete = sched_ch[i];
                    if !discrete.is_nan() {
                        for filter in filters.iter_mut() {
                            filter.update(discrete);
                        }
                    }
                    let idx = offset + i;
                    let in_sample = inp[idx];
                    let mut filtered = in_sample + feedback[i] * fb;
                    for filter in filters.iter_mut() {
                        filtered = filter.process(filtered);
                    }
                    fb = filtered;
                    out[idx] = in_sample + depth[i] * (filtered - in_sample) * 0.5;
                }
                state.feedback_state[ch] = fb;
            }

            offset += n;
        }

        ProcessStatus::Normal
    }

    fn editor(params: Arc<PhaserParams>) -> Box<dyn Editor> {
        GridLayout::build(vec![
            widgets(vec![
                knob(P::Depth, "Depth"),
                knob(P::Feedback, "Fbk"),
                dropdown(P::Stages, "Stages"),
            ]),
            widgets(vec![
                knob(P::MinFreq, "Min Hz"),
                knob(P::SweepWidth, "Sweep"),
                knob(P::LfoRate, "Rate"),
                dropdown(P::Waveform, "Wave"),
                toggle(P::Stereo, "Stereo"),
            ]),
        ])
        .with_title("PHASER")
        .into_editor(&params)
    }
}

truce::plugin! {
    logic: Phaser,
    params: PhaserParams,
}

truce::enable_rt_paranoid!();

#[cfg(test)]
mod rt_paranoid_tests {
    use super::*;
    use std::time::Duration;
    use truce_test::{InputSource, assert_realtime_clean, driver};

    /// `process` makes no allocation, free, or truce-typed lock on the audio
    /// thread. Meaningful under `--features rt-paranoid`; vacuous otherwise.
    #[test]
    fn process_is_realtime_clean() {
        assert_realtime_clean(|| {
            driver!(Plugin)
                .duration(Duration::from_millis(40))
                .input(InputSource::Constant(0.5))
                .run()
        });
    }
}
