//! Phaser - cascade of first-order allpass filters whose centre
//! frequency is swept by an LFO.
//!
//! Ported from Juan Gil's "Audio Effects" Phaser. The allpass form
//! is the one from the book:
//!   `b0 = tan(wc/2) - 1, b1 = tan(wc/2) + 1`
//!   `a0 = tan(wc/2) + 1, a1 = tan(wc/2) - 1`
//! Coefficients are updated every `UPDATE_INTERVAL` samples to keep
//! the audio thread off `tan()` on every sample.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, toggle, widgets};

use PhaserParamsParamId as P;

const MAX_FILTERS_PER_CHANNEL: usize = 10;
const UPDATE_INTERVAL: u32 = 32;

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

pub struct Phaser {
    params: Arc<PhaserParams>,
    sample_rate: f32,
    inv_sr: f32,
    filters: [[Allpass; MAX_FILTERS_PER_CHANNEL]; 2],
    feedback_state: [f32; 2],
    lfo_phase: f32,
    sample_counter: u32,
}

impl Phaser {
    pub fn new(params: Arc<PhaserParams>) -> Self {
        Self {
            params,
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
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        {
            self.sample_rate = sample_rate as f32;
            self.inv_sr = 1.0 / self.sample_rate;
        }
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        self.filters = [[Allpass::default(); MAX_FILTERS_PER_CHANNEL]; 2];
        self.feedback_state = [0.0; 2];
        self.lfo_phase = 0.0;
        self.sample_counter = 0;
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let n = buffer.num_samples();
        let waveform = self.params.waveform.value();
        let stereo = self.params.stereo.value();
        let num_filters = self.params.stages.value().count();

        let mut counter = self.sample_counter;
        let mut phase_main = self.lfo_phase;

        for ch in 0..buffer.channels().min(self.filters.len()) {
            let (inp, out) = buffer.io(ch);
            let mut phase = self.lfo_phase;
            if stereo && ch != 0 {
                phase = (phase + 0.25).rem_euclid(1.0);
            }
            counter = self.sample_counter;

            for i in 0..n {
                let depth = self.params.depth.read();
                let feedback = self.params.feedback.read();
                let min_f = self.params.min_freq.read();
                let sweep = self.params.sweep_width.read();
                let rate = self.params.lfo_rate.read();

                let centre = min_f + sweep * lfo(phase, waveform);
                phase += rate * self.inv_sr;
                if phase >= 1.0 {
                    phase -= 1.0;
                }

                if counter % UPDATE_INTERVAL == 0 {
                    let discrete = std::f32::consts::TAU * centre * self.inv_sr;
                    for filter in &mut self.filters[ch][..num_filters] {
                        filter.update(discrete);
                    }
                }
                counter = counter.wrapping_add(1);

                let mut filtered = inp[i] + feedback * self.feedback_state[ch];
                for filter in &mut self.filters[ch][..num_filters] {
                    filtered = filter.process(filtered);
                }
                self.feedback_state[ch] = filtered;
                out[i] = inp[i] + depth * (filtered - inp[i]) * 0.5;
            }

            if ch == 0 {
                phase_main = phase;
            }
        }

        self.lfo_phase = phase_main;
        self.sample_counter = counter;
        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
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
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Phaser,
    params: PhaserParams,
}
