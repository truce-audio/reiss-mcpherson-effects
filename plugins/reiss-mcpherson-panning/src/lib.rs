//! Panning - two stereo-pan algorithms from Reiss/McPherson:
//!
//! - **Panorama + Precedence**: tangent-law gain + per-side time
//!   delay derived from the pan position. Good for loudspeakers.
//! - **ITD + ILD**: spherical-head model that produces an Interaural
//!   Time Difference (via fractional delay) and an Interaural Level
//!   Difference (via a first-order shelf). Good for headphones.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, widgets};

use PanningParamsParamId as P;

#[derive(ParamEnum)]
pub enum Method {
    #[name = "Pan+Pre"]
    PanoramaPrecedence,
    #[name = "ITD+ILD"]
    ItdIld,
}

#[derive(Params)]
pub struct PanningParams {
    #[param(name = "Method", default = 1)]
    pub method: EnumParam<Method>,

    #[param(
        name = "Pan",
        range = "linear(-1.0, 1.0)",
        default = 0.5,
        smooth = "exp(5)"
    )]
    pub pan: FloatParam,
}

struct Delay {
    buf: Vec<f32>,
    write: usize,
}

impl Delay {
    fn new(max_samples: usize) -> Self {
        let len = max_samples + 2;
        Self {
            buf: vec![0.0; len.max(2)],
            write: 0,
        }
    }
    fn write(&mut self, sample: f32) {
        let len = self.buf.len();
        self.buf[self.write] = sample;
        self.write += 1;
        if self.write >= len {
            self.write -= len;
        }
    }
    fn read(&self, delay_samples: f32) -> f32 {
        let len = self.buf.len();
        #[allow(clippy::cast_precision_loss)]
        let len_f = len as f32;
        #[allow(clippy::cast_precision_loss)]
        let read_pos = (self.write as f32 - 1.0 - delay_samples + len_f).rem_euclid(len_f);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let idx = read_pos.floor() as usize % len;
        let frac = read_pos - read_pos.floor();
        let a = self.buf[idx];
        let b = self.buf[(idx + 1) % len];
        a + frac * (b - a)
    }
}

#[derive(Default, Clone, Copy)]
struct ShelfFilter {
    b0: f32,
    b1: f32,
    a1: f32,
    x1: f32,
    y1: f32,
}

impl ShelfFilter {
    fn update(&mut self, angle: f32, head_factor: f32) {
        let alpha = 1.0 + angle.cos();
        let a0 = head_factor + 1.0;
        let inv = 1.0 / a0;
        self.b0 = (head_factor + alpha) * inv;
        self.b1 = (head_factor - alpha) * inv;
        self.a1 = (head_factor - 1.0) * inv;
    }
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 - self.a1 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
    fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

pub struct Panning;

pub struct PanningDsp {
    sample_rate: f32,
    max_delay_samples: usize,
    delay_l: Delay,
    delay_r: Delay,
    filter_l: ShelfFilter,
    filter_r: ShelfFilter,
}

impl Default for PanningDsp {
    fn default() -> Self {
        Self {
            sample_rate: 44_100.0,
            max_delay_samples: 1,
            delay_l: Delay::new(1),
            delay_r: Delay::new(1),
            filter_l: ShelfFilter::default(),
            filter_r: ShelfFilter::default(),
        }
    }
}

impl PluginLogic for Panning {
    type Params = PanningParams;
    type DspState = PanningDsp;

    fn bus_layouts() -> Vec<BusLayout> {
        // Stereo only: an auto-panner positions the signal across the L/R
        // field, which mono has no room for.
        vec![BusLayout::stereo()]
    }

    fn reset(state: &mut Self::DspState, _params: &Self::Params, config: &AudioConfig) {
        #[allow(clippy::cast_possible_truncation)]
        let sr = config.sample_rate as f32;
        state.sample_rate = sr;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let len = (1e-3 * sr) as usize;
        state.max_delay_samples = len.max(1);
        state.delay_l = Delay::new(state.max_delay_samples);
        state.delay_r = Delay::new(state.max_delay_samples);
        state.filter_l.reset();
        state.filter_r.reset();
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        if buffer.channels() < 2 {
            return ProcessStatus::Normal;
        }
        let n = buffer.num_samples();
        let method = params.method.value();
        #[allow(clippy::cast_precision_loss)]
        let max_delay_f = state.max_delay_samples as f32;
        let sr = state.sample_rate;
        let hp = std::f32::consts::FRAC_PI_2;

        match method {
            Method::PanoramaPrecedence => {
                // theta is fixed at 30° so its sin/cos hoist out;
                // phi tracks the smoothed pan per sample.
                let theta = 30.0_f32.to_radians();
                let (st, ct) = theta.sin_cos();
                for i in 0..n {
                    let pan = params.pan.read();
                    let phi = -pan * theta;
                    let (sp, cp) = phi.sin_cos();
                    let gain_l = cp * st + sp * ct;
                    let gain_r = cp * st - sp * ct;
                    let norm = 1.0 / (gain_l * gain_l + gain_r * gain_r).sqrt();
                    let delay_factor = (pan + 1.0) * 0.5;
                    let delay_l = max_delay_f * delay_factor;
                    let delay_r = max_delay_f * (1.0 - delay_factor);

                    let in_sample = buffer.io(0).0[i];
                    state.delay_l.write(in_sample);
                    state.delay_r.write(in_sample);
                    buffer.io(0).1[i] = state.delay_l.read(delay_l) * gain_l * norm;
                    buffer.io(1).1[i] = state.delay_r.read(delay_r) * gain_r * norm;
                }
            }
            Method::ItdIld => {
                let head_radius = 8.5e-2_f32;
                let speed_of_sound = 340.0_f32;
                let head_factor = sr * head_radius / speed_of_sound;
                let head_factor_shelf = head_radius / speed_of_sound;
                let theta = 90.0_f32.to_radians();
                let td = |angle: f32| -> f32 {
                    if angle.abs() < hp {
                        head_factor * (1.0 - angle.cos())
                    } else {
                        head_factor * (angle.abs() + 1.0 - hp)
                    }
                };

                for i in 0..n {
                    let pan = params.pan.read();
                    let phi = pan * theta;
                    let d_l = td(phi + hp);
                    let d_r = td(phi - hp);
                    state.filter_l.update(phi + hp, head_factor_shelf);
                    state.filter_r.update(phi - hp, head_factor_shelf);

                    let in_sample = buffer.io(0).0[i];
                    state.delay_l.write(in_sample);
                    state.delay_r.write(in_sample);
                    let dl = state.delay_l.read(d_l);
                    let dr = state.delay_r.read(d_r);
                    buffer.io(0).1[i] = state.filter_l.process(dl);
                    buffer.io(1).1[i] = state.filter_r.process(dr);
                }
            }
        }

        ProcessStatus::Normal
    }

    fn editor(params: Arc<PanningParams>) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            dropdown(P::Method, "Method").cols(2),
            knob(P::Pan, "Pan"),
        ])])
        .with_title("PANNING")
        .with_cols(3)
        .into_editor(&params)
    }
}

truce::plugin! {
    logic: Panning,
    params: PanningParams,
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
