//! Compressor / Expander - dynamics processor with four modes
//! (compressor / limiter / expander / noise gate) selected via the
//! Mode dropdown plus Ratio.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, toggle, widgets};

use CompressorParamsParamId as P;

#[derive(ParamEnum)]
pub enum Mode {
    #[name = "Compressor"]
    Compressor,
    #[name = "Expander"]
    Expander,
}

#[derive(Params)]
pub struct CompressorParams {
    #[param(name = "Mode", default = 0)]
    pub mode: EnumParam<Mode>,

    #[param(
        name = "Threshold",
        short_name = "Thresh",
        range = "linear(-60.0, 0.0)",
        default = -24.0,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub threshold: FloatParam,

    #[param(
        name = "Ratio",
        range = "linear(1.0, 100.0)",
        default = 50.0,
        smooth = "exp(5)"
    )]
    pub ratio: FloatParam,

    #[param(
        name = "Attack",
        range = "linear(0.0001, 0.1)",
        default = 0.002,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub attack: FloatParam,

    #[param(
        name = "Release",
        range = "linear(0.01, 1.0)",
        default = 0.3,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub release: FloatParam,

    #[param(
        name = "Makeup",
        range = "linear(-12.0, 12.0)",
        default = 0.0,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub makeup: FloatParam,

    #[param(name = "Bypass", default = 0)]
    pub bypass: BoolParam,
}

pub struct Compressor {
    params: Arc<CompressorParams>,
    inv_sr: f32,
    input_level: f32,
    yl_prev: f32,
}

impl Compressor {
    pub fn new(params: Arc<CompressorParams>) -> Self {
        Self {
            params,
            inv_sr: 1.0 / 44_100.0,
            input_level: 0.0,
            yl_prev: 0.0,
        }
    }
}

const INV_E: f32 = 0.367_879_45;

fn attack_release_coeff(value_s: f32, inv_sr: f32) -> f32 {
    if value_s <= 0.0 {
        0.0
    } else {
        INV_E.powf(inv_sr / value_s)
    }
}

impl PluginLogic for Compressor {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        {
            self.inv_sr = 1.0 / sample_rate as f32;
        }
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        self.input_level = 0.0;
        self.yl_prev = 0.0;
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        if self.params.bypass.value() {
            // Copy input → output without touching the dynamics
            // state; on the next un-bypassed block the envelope
            // continues smoothly from `input_level`.
            for ch in 0..buffer.channels() {
                let (inp, out) = buffer.io(ch);
                out.copy_from_slice(inp);
            }
            return ProcessStatus::Normal;
        }

        let n = buffer.num_samples();
        let num_ch = buffer.channels();
        if num_ch == 0 {
            return ProcessStatus::Normal;
        }
        let expander = matches!(self.params.mode.value(), Mode::Expander);
        #[allow(clippy::cast_precision_loss)]
        let inv_ch = 1.0_f32 / num_ch as f32;

        for i in 0..n {
            let t = self.params.threshold.read();
            let r = self.params.ratio.read();
            let alpha_a = attack_release_coeff(self.params.attack.read(), self.inv_sr);
            let alpha_r = attack_release_coeff(self.params.release.read(), self.inv_sr);
            let makeup = self.params.makeup.read();

            // Mono mixdown of the input for detection.
            let mut mix = 0.0_f32;
            for ch in 0..num_ch {
                mix += buffer.io(ch).0[i];
            }
            mix *= inv_ch;
            let in_squared = mix * mix;

            if expander {
                // Slow integration on the squared input - the book's
                // recipe to suppress the gain pumping that an instant
                // RMS would cause on noise-gate / expander curves.
                const AVERAGE_FACTOR: f32 = 0.9999;
                self.input_level =
                    AVERAGE_FACTOR * self.input_level + (1.0 - AVERAGE_FACTOR) * in_squared;
            } else {
                self.input_level = in_squared;
            }

            let xg = if self.input_level <= 1e-6 {
                -60.0
            } else {
                10.0 * self.input_level.log10()
            };

            let (yg, xl);
            if expander {
                yg = if xg > t { xg } else { t + (xg - t) * r };
                xl = xg - yg;
            } else {
                yg = if xg < t { xg } else { t + (xg - t) / r };
                xl = xg - yg;
            }

            // Asymmetric one-pole smoothing: rising side uses attack
            // for compressors / release for expanders, mirrored on
            // the falling side.
            let yl = if expander {
                if xl < self.yl_prev {
                    alpha_a * self.yl_prev + (1.0 - alpha_a) * xl
                } else {
                    alpha_r * self.yl_prev + (1.0 - alpha_r) * xl
                }
            } else if xl > self.yl_prev {
                alpha_a * self.yl_prev + (1.0 - alpha_a) * xl
            } else {
                alpha_r * self.yl_prev + (1.0 - alpha_r) * xl
            };

            let control = 10f32.powf((makeup - yl) * 0.05);
            self.yl_prev = yl;

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                out[i] = inp[i] * control;
            }
        }

        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![
            widgets(vec![
                dropdown(P::Mode, "Mode").cols(2),
                knob(P::Threshold, "Thresh"),
                knob(P::Ratio, "Ratio"),
            ]),
            widgets(vec![
                knob(P::Attack, "Atk"),
                knob(P::Release, "Rel"),
                knob(P::Makeup, "Makeup"),
                toggle(P::Bypass, "Bypass"),
            ]),
        ])
        .with_title("COMP/EXP")
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Compressor,
    params: CompressorParams,
}
