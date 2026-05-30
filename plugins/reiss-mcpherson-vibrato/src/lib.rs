//! Vibrato - LFO-modulated delay producing periodic pitch variation.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, widgets};

use VibratoParamsParamId as P;

const MAX_WIDTH_SECS: f32 = 0.05;
const MAX_BLOCK: usize = 512;

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
pub struct VibratoParams {
    #[param(
        name = "Width",
        range = "linear(0.001, 0.05)",
        default = 0.01,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub width: FloatParam,

    #[param(
        name = "LFO Rate",
        short_name = "Rate",
        range = "linear(0.0, 10.0)",
        default = 2.0,
        unit = "Hz",
        smooth = "exp(5)"
    )]
    pub rate: FloatParam,

    #[param(name = "Waveform", default = 0)]
    pub waveform: EnumParam<Waveform>,

    #[param(name = "Interp", default = 1)]
    pub interp: EnumParam<Interpolation>,
}

pub struct Vibrato {
    params: Arc<VibratoParams>,
    sample_rate: f32,
    buffer: Vec<Vec<f32>>,
    buffer_len: usize,
    write_pos: usize,
    lfo_phase: f32,
}

impl Vibrato {
    pub fn new(params: Arc<VibratoParams>) -> Self {
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

impl PluginLogic for Vibrato {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        let sr = sample_rate as f32;
        self.sample_rate = sr;
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let len = (MAX_WIDTH_SECS * sr) as usize + 1;
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
        let total = buffer.num_samples();
        let buf_len = self.buffer_len;
        #[allow(clippy::cast_precision_loss)]
        let buf_len_f = buf_len as f32;
        let waveform = self.params.waveform.value();
        let interp = self.params.interp.value();
        let num_ch = buffer.channels().min(self.buffer.len());

        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(MAX_BLOCK);

            let width = self.params.width.read_block::<MAX_BLOCK>();
            let rate = self.params.rate.read_block::<MAX_BLOCK>();

            // Precompute the read trajectory once - channels share
            // it, and the inner loop becomes pure stack-array reads.
            let mut read_idx = [0usize; MAX_BLOCK];
            let mut frac_arr = [0.0_f32; MAX_BLOCK];
            let mut write_idx = [0usize; MAX_BLOCK];
            for i in 0..n {
                let delay_samples =
                    width[i] * lfo(self.lfo_phase, waveform) * self.sample_rate;
                let write = self.write_pos;
                #[allow(clippy::cast_precision_loss)]
                let read_pos =
                    (write as f32 - delay_samples + buf_len_f - 1.0).rem_euclid(buf_len_f);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let r0 = read_pos.floor() as usize % buf_len;
                read_idx[i] = r0;
                frac_arr[i] = read_pos - read_pos.floor();
                write_idx[i] = write;
                self.write_pos += 1;
                if self.write_pos >= buf_len {
                    self.write_pos -= buf_len;
                }
                self.lfo_phase += rate[i] / self.sample_rate;
                if self.lfo_phase >= 1.0 {
                    self.lfo_phase -= 1.0;
                }
            }

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                let line = &mut self.buffer[ch];
                for i in 0..n {
                    let idx = offset + i;
                    let in_sample = inp[idx];
                    let r0 = read_idx[i];
                    let frac = frac_arr[i];
                    let out_sample = match interp {
                        Interpolation::Nearest => line[r0],
                        Interpolation::Linear => {
                            let s0 = line[r0];
                            let s1 = line[(r0 + 1) % buf_len];
                            s0 + frac * (s1 - s0)
                        }
                        Interpolation::Cubic => {
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
                    line[write_idx[i]] = in_sample;
                    out[idx] = out_sample;
                }
            }

            offset += n;
        }

        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            knob(P::Width, "Width"),
            knob(P::Rate, "Rate"),
            dropdown(P::Waveform, "Wave"),
            dropdown(P::Interp, "Interp"),
        ])])
        .with_title("VIBRATO")
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Vibrato,
    params: VibratoParams,
}
