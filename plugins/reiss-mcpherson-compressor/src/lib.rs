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

pub struct Compressor;

pub struct CompressorDsp {
    inv_sr: f32,
    input_level: f32,
    yl_prev: f32,
}

impl Default for CompressorDsp {
    fn default() -> Self {
        Self {
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
    type Params = CompressorParams;
    type DspState = CompressorDsp;

    fn reset(state: &mut Self::DspState, _params: &Self::Params, config: &AudioConfig) {
        #[allow(clippy::cast_possible_truncation)]
        {
            state.inv_sr = 1.0 / config.sample_rate as f32;
        }
        state.input_level = 0.0;
        state.yl_prev = 0.0;
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        if params.bypass.value() {
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
        let expander = matches!(params.mode.value(), Mode::Expander);
        #[allow(clippy::cast_precision_loss)]
        let inv_ch = 1.0_f32 / num_ch as f32;

        for i in 0..n {
            let t = params.threshold.read();
            let r = params.ratio.read();
            let alpha_a = attack_release_coeff(params.attack.read(), state.inv_sr);
            let alpha_r = attack_release_coeff(params.release.read(), state.inv_sr);
            let makeup = params.makeup.read();

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
                state.input_level =
                    AVERAGE_FACTOR * state.input_level + (1.0 - AVERAGE_FACTOR) * in_squared;
            } else {
                state.input_level = in_squared;
            }

            let xg = if state.input_level <= 1e-6 {
                -60.0
            } else {
                10.0 * state.input_level.log10()
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
                if xl < state.yl_prev {
                    alpha_a * state.yl_prev + (1.0 - alpha_a) * xl
                } else {
                    alpha_r * state.yl_prev + (1.0 - alpha_r) * xl
                }
            } else if xl > state.yl_prev {
                alpha_a * state.yl_prev + (1.0 - alpha_a) * xl
            } else {
                alpha_r * state.yl_prev + (1.0 - alpha_r) * xl
            };

            let control = 10f32.powf((makeup - yl) * 0.05);
            state.yl_prev = yl;

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                out[i] = inp[i] * control;
            }
        }

        ProcessStatus::Normal
    }

    fn editor(params: Arc<CompressorParams>) -> Box<dyn Editor> {
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
        .into_editor(&params)
    }
}

truce::plugin! {
    logic: Compressor,
    params: CompressorParams,
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
