//! Parametric EQ - single-band selectable filter.
//!
//! Ported from Juan Gil's "Audio Effects" Parametric EQ. Implements
//! seven filter shapes from the book (first-order LP/HP/LS/HS plus
//! second-order BP/BS/Peak). Coefficients map JUCE's
//! `b0 + b1 z^-1 + b2 z^-2 / a0 + a1 z^-1 + a2 z^-2` form directly,
//! normalised by a0 at evaluation time.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, widgets};

use ParametricEqParamsParamId as P;

#[derive(ParamEnum)]
pub enum FilterType {
    #[name = "Low-Pass"]
    LowPass,
    #[name = "High-Pass"]
    HighPass,
    #[name = "Low-Shelf"]
    LowShelf,
    #[name = "High-Shelf"]
    HighShelf,
    #[name = "Band-Pass"]
    BandPass,
    #[name = "Band-Stop"]
    BandStop,
    Peaking,
}

#[derive(Params)]
pub struct ParametricEqParams {
    #[param(
        name = "Frequency",
        short_name = "Freq",
        range = "log(10.0, 20000.0)",
        default = 1500.0,
        unit = "Hz",
        smooth = "exp(10)"
    )]
    pub frequency: FloatParam,

    #[param(
        name = "Q",
        range = "linear(0.1, 20.0)",
        default = 1.4142135,
        smooth = "exp(10)"
    )]
    pub q: FloatParam,

    #[param(
        name = "Gain",
        range = "linear(-12.0, 12.0)",
        default = 0.0,
        unit = "dB",
        smooth = "exp(10)"
    )]
    pub gain: FloatParam,

    #[param(name = "Type", default = 6)]
    pub filter_type: EnumParam<FilterType>,
}

/// Direct-form-I biquad state for a single channel, normalized at
/// coefficient-update time so the inner loop is the canonical
/// `b0 x[n] + b1 x[n-1] + b2 x[n-2] - a1 y[n-1] - a2 y[n-2]`.
#[derive(Default, Clone)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn set(&mut self, b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) {
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

fn update(filter: &mut Biquad, freq_hz: f64, q: f64, gain_lin: f64, ty: FilterType, sr: f64) {
    let wc = (2.0 * std::f64::consts::PI * freq_hz / sr).clamp(1e-6, std::f64::consts::PI * 0.99);
    let bw = (wc / q).min(std::f64::consts::PI * 0.99);
    let two_cos_wc = -2.0 * wc.cos();
    let tan_half_bw = (bw / 2.0).tan();
    let tan_half_wc = (wc / 2.0).tan();
    let sqrt_g = gain_lin.sqrt();
    let g = gain_lin;

    match ty {
        FilterType::LowPass => filter.set(
            tan_half_wc,
            tan_half_wc,
            0.0,
            tan_half_wc + 1.0,
            tan_half_wc - 1.0,
            0.0,
        ),
        FilterType::HighPass => {
            filter.set(1.0, -1.0, 0.0, tan_half_wc + 1.0, tan_half_wc - 1.0, 0.0);
        }
        FilterType::LowShelf => filter.set(
            g * tan_half_wc + sqrt_g,
            g * tan_half_wc - sqrt_g,
            0.0,
            tan_half_wc + sqrt_g,
            tan_half_wc - sqrt_g,
            0.0,
        ),
        FilterType::HighShelf => filter.set(
            sqrt_g * tan_half_wc + g,
            sqrt_g * tan_half_wc - g,
            0.0,
            sqrt_g * tan_half_wc + 1.0,
            sqrt_g * tan_half_wc - 1.0,
            0.0,
        ),
        FilterType::BandPass => filter.set(
            tan_half_bw,
            0.0,
            -tan_half_bw,
            1.0 + tan_half_bw,
            two_cos_wc,
            1.0 - tan_half_bw,
        ),
        FilterType::BandStop => filter.set(
            1.0,
            two_cos_wc,
            1.0,
            1.0 + tan_half_bw,
            two_cos_wc,
            1.0 - tan_half_bw,
        ),
        FilterType::Peaking => filter.set(
            sqrt_g + g * tan_half_bw,
            sqrt_g * two_cos_wc,
            sqrt_g - g * tan_half_bw,
            sqrt_g + tan_half_bw,
            sqrt_g * two_cos_wc,
            sqrt_g - tan_half_bw,
        ),
    }
}

pub struct ParametricEq {
    params: Arc<ParametricEqParams>,
    sample_rate: f64,
    filters: [Biquad; 2],
    last_freq: f64,
    last_q: f64,
    last_gain_db: f64,
    last_type: i32,
}

impl ParametricEq {
    pub fn new(params: Arc<ParametricEqParams>) -> Self {
        Self {
            params,
            sample_rate: 44_100.0,
            filters: Default::default(),
            last_freq: f64::NAN,
            last_q: f64::NAN,
            last_gain_db: f64::NAN,
            last_type: i32::MIN,
        }
    }
}

impl PluginLogic for ParametricEq {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        self.sample_rate = sample_rate;
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        for f in &mut self.filters {
            f.reset();
        }
        self.last_freq = f64::NAN;
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let n = buffer.num_samples();
        let ty = self.params.filter_type.value();
        // Rebuild biquad coefficients per block, not per sample - the
        // JUCE source updates them inside parameter setters, never on
        // smoothed values. Smoothing the parameter just animates the
        // host-side value; the filter snaps at block boundaries.
        let freq = f64::from(self.params.frequency.read());
        let q = f64::from(self.params.q.read());
        let gain_db = f64::from(self.params.gain.read());
        #[allow(clippy::cast_possible_truncation)]
        let ty_idx = ty as i32;

        let changed = (freq - self.last_freq).abs() > 1e-6
            || (q - self.last_q).abs() > 1e-6
            || (gain_db - self.last_gain_db).abs() > 1e-6
            || ty_idx != self.last_type;
        if changed {
            let gain_lin = 10f64.powf(gain_db * 0.05);
            for f in &mut self.filters {
                update(f, freq, q, gain_lin, ty, self.sample_rate);
            }
            self.last_freq = freq;
            self.last_q = q;
            self.last_gain_db = gain_db;
            self.last_type = ty_idx;
        }

        for ch in 0..buffer.channels().min(self.filters.len()) {
            let (inp, out) = buffer.io(ch);
            let f = &mut self.filters[ch];
            for i in 0..n {
                #[allow(clippy::cast_possible_truncation)]
                {
                    out[i] = f.process(f64::from(inp[i])) as f32;
                }
            }
        }

        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            knob(P::Frequency, "Freq"),
            knob(P::Q, "Q"),
            knob(P::Gain, "Gain"),
            dropdown(P::FilterType, "Type"),
        ])])
        .with_title("PARAMETRIC EQ")
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: ParametricEq,
    params: ParametricEqParams,
}
