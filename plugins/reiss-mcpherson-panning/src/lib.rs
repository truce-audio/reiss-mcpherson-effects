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

pub struct Panning {
    params: Arc<PanningParams>,
    sample_rate: f32,
    max_delay_samples: usize,
    delay_l: Delay,
    delay_r: Delay,
    filter_l: ShelfFilter,
    filter_r: ShelfFilter,
}

impl Panning {
    pub fn new(params: Arc<PanningParams>) -> Self {
        Self {
            params,
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
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        let sr = sample_rate as f32;
        self.sample_rate = sr;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let len = (1e-3 * sr) as usize;
        self.max_delay_samples = len.max(1);
        self.delay_l = Delay::new(self.max_delay_samples);
        self.delay_r = Delay::new(self.max_delay_samples);
        self.filter_l.reset();
        self.filter_r.reset();
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        if buffer.channels() < 2 {
            return ProcessStatus::Normal;
        }
        let n = buffer.num_samples();
        let method = self.params.method.value();
        #[allow(clippy::cast_precision_loss)]
        let max_delay_f = self.max_delay_samples as f32;
        let sr = self.sample_rate;
        let hp = std::f32::consts::FRAC_PI_2;

        match method {
            Method::PanoramaPrecedence => {
                // theta is fixed at 30° so its sin/cos hoist out;
                // phi tracks the smoothed pan per sample.
                let theta = 30.0_f32.to_radians();
                let (st, ct) = theta.sin_cos();
                for i in 0..n {
                    let pan = self.params.pan.read();
                    let phi = -pan * theta;
                    let (sp, cp) = phi.sin_cos();
                    let gain_l = cp * st + sp * ct;
                    let gain_r = cp * st - sp * ct;
                    let norm = 1.0 / (gain_l * gain_l + gain_r * gain_r).sqrt();
                    let delay_factor = (pan + 1.0) * 0.5;
                    let delay_l = max_delay_f * delay_factor;
                    let delay_r = max_delay_f * (1.0 - delay_factor);

                    let in_sample = buffer.io(0).0[i];
                    self.delay_l.write(in_sample);
                    self.delay_r.write(in_sample);
                    buffer.io(0).1[i] = self.delay_l.read(delay_l) * gain_l * norm;
                    buffer.io(1).1[i] = self.delay_r.read(delay_r) * gain_r * norm;
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
                    let pan = self.params.pan.read();
                    let phi = pan * theta;
                    let d_l = td(phi + hp);
                    let d_r = td(phi - hp);
                    self.filter_l.update(phi + hp, head_factor_shelf);
                    self.filter_r.update(phi - hp, head_factor_shelf);

                    let in_sample = buffer.io(0).0[i];
                    self.delay_l.write(in_sample);
                    self.delay_r.write(in_sample);
                    let dl = self.delay_l.read(d_l);
                    let dr = self.delay_r.read(d_r);
                    buffer.io(0).1[i] = self.filter_l.process(dl);
                    buffer.io(1).1[i] = self.filter_r.process(dr);
                }
            }
        }

        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            dropdown(P::Method, "Method").cols(2),
            knob(P::Pan, "Pan"),
        ])])
        .with_title("PANNING")
        .with_cols(3)
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Panning,
    params: PanningParams,
}
