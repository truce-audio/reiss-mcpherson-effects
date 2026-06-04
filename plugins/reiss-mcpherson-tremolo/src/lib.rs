//! Tremolo - LFO-modulated amplitude.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, widgets};

use TremoloParamsParamId as P;

/// Upper bound on samples per inner chunk. Each smoothed param gets
/// a `[f32; MAX_BLOCK]` scratch so the channel-major hot loop reads
/// from a stack array instead of calling `.read()` per sample.
const MAX_BLOCK: usize = 512;

#[derive(ParamEnum)]
pub enum Waveform {
    Sine,
    Triangle,
    Sawtooth,
    #[name = "Inv. Saw"]
    InverseSawtooth,
    Square,
    #[name = "Sq. Sloped"]
    SquareSloped,
}

#[derive(Params)]
pub struct TremoloParams {
    #[param(
        name = "Depth",
        range = "linear(0.0, 1.0)",
        default = 0.5,
        smooth = "exp(5)"
    )]
    pub depth: FloatParam,

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
}

pub struct Tremolo {
    params: Arc<TremoloParams>,
    inv_sr: f32,
    lfo_phase: f32,
}

impl Tremolo {
    pub fn new(params: Arc<TremoloParams>) -> Self {
        Self {
            params,
            inv_sr: 1.0 / 44_100.0,
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
        Waveform::Square => {
            if phase < 0.5 {
                0.0
            } else {
                1.0
            }
        }
        Waveform::SquareSloped => {
            // Square with short ramped edges; eliminates audible
            // discontinuities at the transitions.
            if phase < 0.48 {
                1.0
            } else if phase < 0.5 {
                1.0 - 50.0 * (phase - 0.48)
            } else if phase < 0.98 {
                0.0
            } else {
                50.0 * (phase - 0.98)
            }
        }
    }
}

impl PluginLogic for Tremolo {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        {
            self.inv_sr = 1.0 / sample_rate as f32;
        }
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        self.lfo_phase = 0.0;
    }

    // Hot-path loops index multiple stack arrays (gain, depth, rate,
    // in/out at offset + i) by a shared `i`. Iterator+zip chains
    // would obscure the channel-major shape we're optimising for.
    #[allow(clippy::needless_range_loop)]
    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let total = buffer.num_samples();
        let waveform = self.params.waveform.value();
        let num_ch = buffer.channels();

        // Walk the buffer in MAX_BLOCK chunks. Per chunk: one
        // block-read advances the smoother by exactly N samples,
        // we precompute the LFO modulation curve once, then
        // channel-major loops iterate the inner samples (the
        // shape LLVM is happiest vectorising).
        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(MAX_BLOCK);

            let depth = self.params.depth.read_block::<MAX_BLOCK>();
            let rate = self.params.rate.read_block::<MAX_BLOCK>();

            let mut gain = [0.0_f32; MAX_BLOCK];
            for i in 0..n {
                let m = lfo(self.lfo_phase, waveform);
                gain[i] = 1.0 - depth[i] + depth[i] * m;
                self.lfo_phase += rate[i] * self.inv_sr;
                if self.lfo_phase >= 1.0 {
                    self.lfo_phase -= 1.0;
                }
            }

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                for i in 0..n {
                    let idx = offset + i;
                    out[idx] = inp[idx] * gain[i];
                }
            }

            offset += n;
        }
        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            knob(P::Depth, "Depth"),
            knob(P::Rate, "Rate"),
            dropdown(P::Waveform, "Wave").cols(2),
        ])])
        .with_title("TREMOLO")
        .with_cols(4)
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Tremolo,
    params: TremoloParams,
}
