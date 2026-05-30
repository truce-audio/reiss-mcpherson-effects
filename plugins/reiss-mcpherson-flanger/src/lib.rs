//! Flanger - short modulated delay summed with the dry signal.
//!
//! Ported from Juan Gil's "Audio Effects" Flanger. The delay time
//! sweeps between `Delay` and `Delay + Width` driven by an LFO.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, toggle, widgets};

use FlangerParamsParamId as P;

const MAX_DELAY_SECS: f32 = 0.04;

#[derive(ParamEnum)]
pub enum Waveform {
    Sine,
    Triangle,
    Sawtooth,
    #[name = "Inv. Sawtooth"]
    InverseSawtooth,
}

#[derive(ParamEnum)]
pub enum Interpolation {
    Nearest,
    Linear,
    Cubic,
}

#[derive(Params)]
pub struct FlangerParams {
    #[param(
        name = "Delay",
        range = "linear(0.001, 0.02)",
        default = 0.0025,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub delay: FloatParam,

    #[param(
        name = "Width",
        range = "linear(0.001, 0.02)",
        default = 0.01,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub width: FloatParam,

    #[param(
        name = "Depth",
        range = "linear(0.0, 1.0)",
        default = 1.0,
        smooth = "exp(5)"
    )]
    pub depth: FloatParam,

    #[param(
        name = "Feedback",
        range = "linear(0.0, 0.5)",
        default = 0.0,
        smooth = "exp(5)"
    )]
    pub feedback: FloatParam,

    #[param(name = "Inverted", default = 0)]
    pub inverted: BoolParam,

    #[param(
        name = "LFO Rate",
        short_name = "Rate",
        range = "linear(0.05, 2.0)",
        default = 0.2,
        unit = "Hz",
        smooth = "exp(5)"
    )]
    pub rate: FloatParam,

    #[param(name = "Waveform", default = 0)]
    pub waveform: EnumParam<Waveform>,

    #[param(name = "Interp", default = 1)]
    pub interp: EnumParam<Interpolation>,

    #[param(name = "Stereo", default = 0)]
    pub stereo: BoolParam,
}

pub struct Flanger {
    params: Arc<FlangerParams>,
    sample_rate: f32,
    buffer: Vec<Vec<f32>>,
    buffer_len: usize,
    write_pos: usize,
    lfo_phase: f32,
}

impl Flanger {
    pub fn new(params: Arc<FlangerParams>) -> Self {
        Self {
            params,
            sample_rate: 44_100.0,
            buffer: Vec::new(),
            buffer_len: 1,
            write_pos: 0,
            lfo_phase: 0.0,
        }
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
        Waveform::Sawtooth => {
            if phase < 0.5 {
                0.5 + phase
            } else {
                phase - 0.5
            }
        }
        Waveform::InverseSawtooth => {
            if phase < 0.5 {
                0.5 - phase
            } else {
                1.5 - phase
            }
        }
    }
}

impl PluginLogic for Flanger {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        let sr = sample_rate as f32;
        self.sample_rate = sr;
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let len = (MAX_DELAY_SECS * sr) as usize + 1;
        self.buffer_len = len.max(4);
        self.buffer = vec![vec![0.0; self.buffer_len]; 2];
        self.write_pos = 0;
        self.lfo_phase = 0.0;
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let n = buffer.num_samples();
        let buf_len = self.buffer_len;
        #[allow(clippy::cast_precision_loss)]
        let buf_len_f = buf_len as f32;
        let waveform = self.params.waveform.value();
        let interp = self.params.interp.value();
        let stereo = self.params.stereo.value();
        let inverted = self.params.inverted.value();
        let invert = if inverted { -1.0_f32 } else { 1.0 };

        let mut final_write = self.write_pos;
        let mut final_phase = self.lfo_phase;

        for ch in 0..buffer.channels().min(self.buffer.len()) {
            let mut write = self.write_pos;
            let mut ph = self.lfo_phase;
            if stereo && ch != 0 {
                ph = (ph + 0.25).rem_euclid(1.0);
            }
            let (inp, out) = buffer.io(ch);
            let line = &mut self.buffer[ch];

            for i in 0..n {
                let delay = self.params.delay.read();
                let width = self.params.width.read();
                let depth = self.params.depth.read();
                let feedback = self.params.feedback.read();
                let rate = self.params.rate.read();
                let delay_samples =
                    (delay + width * lfo(ph, waveform)) * self.sample_rate;

                #[allow(clippy::cast_precision_loss)]
                let read_pos =
                    (write as f32 - delay_samples + buf_len_f).rem_euclid(buf_len_f);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let r0 = read_pos.floor() as usize % buf_len;

                let delayed = match interp {
                    Interpolation::Nearest => line[r0],
                    Interpolation::Linear => {
                        let frac = read_pos - read_pos.floor();
                        let s0 = line[r0];
                        let s1 = line[(r0 + 1) % buf_len];
                        s0 + frac * (s1 - s0)
                    }
                    Interpolation::Cubic => {
                        let frac = read_pos - read_pos.floor();
                        let f2 = frac * frac;
                        let f3 = f2 * frac;
                        let s0 = line[(r0 + buf_len - 1) % buf_len];
                        let s1 = line[r0];
                        let s2 = line[(r0 + 1) % buf_len];
                        let s3 = line[(r0 + 2) % buf_len];
                        let a0 = -0.5 * s0 + 1.5 * s1 - 1.5 * s2 + 0.5 * s3;
                        let a1 = s0 - 2.5 * s1 + 2.0 * s2 - 0.5 * s3;
                        let a2 = -0.5 * s0 + 0.5 * s2;
                        a0 * f3 + a1 * f2 + a2 * frac + s1
                    }
                };

                let in_sample = inp[i];
                out[i] = in_sample + delayed * depth * invert;
                line[write] = in_sample + delayed * feedback;

                write += 1;
                if write >= buf_len {
                    write -= buf_len;
                }
                ph += rate / self.sample_rate;
                if ph >= 1.0 {
                    ph -= 1.0;
                }
            }

            if ch == 0 {
                final_phase = ph;
            }
            final_write = write;
        }

        self.write_pos = final_write;
        self.lfo_phase = final_phase;
        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![
            widgets(vec![
                knob(P::Delay, "Delay"),
                knob(P::Width, "Width"),
                knob(P::Depth, "Depth"),
                knob(P::Feedback, "Fbk"),
            ]),
            widgets(vec![
                knob(P::Rate, "Rate"),
                dropdown(P::Waveform, "Wave"),
                dropdown(P::Interp, "Interp"),
                toggle(P::Inverted, "Inv"),
                toggle(P::Stereo, "Stereo"),
            ]),
        ])
        .with_title("FLANGER")
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Flanger,
    params: FlangerParams,
}
