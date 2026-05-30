//! Chorus - multiple LFO-modulated delay voices summed with dry.
//!
//! Ported from Juan Gil's "Audio Effects" Chorus. 2 to 5 voices are
//! tapped from a shared delay line; voice 1 is the dry signal, the
//! rest each get a phase-offset LFO tap that times-shifts them to
//! simulate ensemble width.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, toggle, widgets};

use ChorusParamsParamId as P;

const MAX_DELAY_SECS: f32 = 0.1;

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
pub struct ChorusParams {
    #[param(
        name = "Delay",
        range = "linear(0.01, 0.05)",
        default = 0.03,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub delay: FloatParam,

    #[param(
        name = "Width",
        range = "linear(0.01, 0.05)",
        default = 0.02,
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

    /// 2..=5 voices (voice 1 is dry; the rest are wet taps).
    #[param(name = "Voices", range = "discrete(2, 5)", default = 2)]
    pub voices: IntParam,

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

    #[param(name = "Stereo", default = 1)]
    pub stereo: BoolParam,
}

pub struct Chorus {
    params: Arc<ChorusParams>,
    sample_rate: f32,
    buffer: Vec<Vec<f32>>,
    buffer_len: usize,
    write_pos: usize,
    lfo_phase: f32,
}

impl Chorus {
    pub fn new(params: Arc<ChorusParams>) -> Self {
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
    let phase = phase.rem_euclid(1.0);
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

fn sample_at(line: &[f32], pos: f32, buf_len: usize, interp: Interpolation) -> f32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let r0 = pos.floor() as usize % buf_len;
    match interp {
        Interpolation::Nearest => line[r0],
        Interpolation::Linear => {
            let frac = pos - pos.floor();
            let s0 = line[r0];
            let s1 = line[(r0 + 1) % buf_len];
            s0 + frac * (s1 - s0)
        }
        Interpolation::Cubic => {
            let frac = pos - pos.floor();
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
    }
}

impl PluginLogic for Chorus {
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
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let num_voices = self.params.voices.value().clamp(2, 5) as usize;

        let mut final_write = self.write_pos;
        let mut final_phase = self.lfo_phase;

        for ch in 0..buffer.channels().min(self.buffer.len()) {
            let mut write = self.write_pos;
            let mut ph = self.lfo_phase;
            let (inp, out) = buffer.io(ch);
            let line = &mut self.buffer[ch];

            for i in 0..n {
                let delay = self.params.delay.read();
                let width = self.params.width.read();
                let depth = self.params.depth.read();
                let rate = self.params.rate.read();

                let in_sample = inp[i];
                let mut acc = in_sample;
                let mut phase_offset = 0.0_f32;

                for voice in 0..num_voices - 1 {
                    let weight = if stereo && num_voices > 2 {
                        #[allow(clippy::cast_precision_loss)]
                        let mut w = voice as f32 / (num_voices - 2) as f32;
                        if ch != 0 {
                            w = 1.0 - w;
                        }
                        w
                    } else {
                        1.0
                    };

                    let local_delay =
                        (delay + width * lfo(ph + phase_offset, waveform)) * self.sample_rate;
                    #[allow(clippy::cast_precision_loss)]
                    let read_pos =
                        (write as f32 - local_delay + buf_len_f).rem_euclid(buf_len_f);
                    let voiced = sample_at(line, read_pos, buf_len, interp);

                    if stereo && num_voices == 2 {
                        // Voice 1 (channel 0) is dry; wet channel is
                        // overwritten with the modulated tap.
                        if ch == 0 {
                            acc = in_sample;
                        } else {
                            acc = voiced * depth;
                        }
                    } else {
                        acc += voiced * depth * weight;
                    }

                    if num_voices == 3 {
                        phase_offset += 0.25;
                    } else if num_voices > 3 {
                        #[allow(clippy::cast_precision_loss)]
                        let step = 1.0_f32 / (num_voices - 1) as f32;
                        phase_offset += step;
                    }
                }

                out[i] = acc;
                line[write] = in_sample;
                write += 1;
                if write >= buf_len {
                    write -= buf_len;
                }
                ph += rate / self.sample_rate;
                if ph >= 1.0 {
                    ph -= 1.0;
                }
            }

            final_write = write;
            final_phase = ph;
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
                knob(P::Voices, "Voices"),
            ]),
            widgets(vec![
                knob(P::Rate, "Rate"),
                dropdown(P::Waveform, "Wave"),
                dropdown(P::Interp, "Interp"),
                toggle(P::Stereo, "Stereo"),
            ]),
        ])
        .with_title("CHORUS")
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Chorus,
    params: ChorusParams,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_is_valid() {
        truce_test::assert_valid_info::<Plugin>();
    }
    #[test]
    fn has_editor() {
        truce_test::assert_has_editor::<Plugin>();
    }
    #[test]
    fn state_round_trips() {
        truce_test::assert_state_round_trip::<Plugin>();
    }
}
