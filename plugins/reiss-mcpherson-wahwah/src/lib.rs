//! Wah-Wah - resonant filter whose centre frequency is moved by an
//! LFO, an input envelope follower, or a manual slider.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, section};

use WahWahParamsParamId as P;

const MAX_BLOCK: usize = 512;

#[derive(ParamEnum)]
pub enum Mode {
    Manual,
    Automatic,
}

#[derive(ParamEnum)]
pub enum FilterType {
    #[name = "Res. LP"]
    ResonantLowPass,
    #[name = "Band-Pass"]
    BandPass,
    Peaking,
}

#[derive(Params)]
pub struct WahWahParams {
    #[param(name = "Mode", default = 0)]
    pub mode: EnumParam<Mode>,

    #[param(
        name = "Mix",
        range = "linear(0.0, 1.0)",
        default = 0.5,
        smooth = "exp(5)"
    )]
    pub mix: FloatParam,

    /// Manual centre frequency; in Automatic mode the live frequency
    /// is the LFO/envelope mix scaled into [min, max].
    #[param(
        name = "Frequency",
        short_name = "Freq",
        range = "log(200.0, 1300.0)",
        default = 300.0,
        unit = "Hz",
        smooth = "exp(5)"
    )]
    pub frequency: FloatParam,

    #[param(
        name = "Q",
        range = "linear(0.1, 20.0)",
        default = 10.0,
        smooth = "exp(5)"
    )]
    pub q: FloatParam,

    #[param(
        name = "Gain",
        range = "linear(0.0, 20.0)",
        default = 20.0,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub gain: FloatParam,

    #[param(name = "Filter Type", short_name = "Type", default = 0)]
    pub filter_type: EnumParam<FilterType>,

    #[param(
        name = "LFO Rate",
        short_name = "Rate",
        range = "linear(0.0, 5.0)",
        default = 2.0,
        unit = "Hz",
        smooth = "exp(5)"
    )]
    pub lfo_rate: FloatParam,

    /// 0 → pure LFO, 1 → pure envelope follower.
    #[param(
        name = "LFO/Env Mix",
        short_name = "LFO/Env",
        range = "linear(0.0, 1.0)",
        default = 0.8,
        smooth = "exp(5)"
    )]
    pub lfo_env_mix: FloatParam,

    #[param(
        name = "Env Attack",
        short_name = "Atk",
        range = "linear(0.0001, 0.1)",
        default = 0.002,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub env_attack: FloatParam,

    #[param(
        name = "Env Release",
        short_name = "Rel",
        range = "linear(0.01, 1.0)",
        default = 0.3,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub env_release: FloatParam,
}

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
    let tw2 = tan_half_wc * tan_half_wc;
    let sqrt_g = gain_lin.sqrt();
    let g = gain_lin;

    match ty {
        FilterType::ResonantLowPass => filter.set(
            tw2,
            2.0 * tw2,
            tw2,
            tw2 + tan_half_wc / g + 1.0,
            2.0 * tw2 - 2.0,
            tw2 - tan_half_wc / g + 1.0,
        ),
        FilterType::BandPass => filter.set(
            tan_half_bw,
            0.0,
            -tan_half_bw,
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

pub struct WahWah {
    params: Arc<WahWahParams>,
    sample_rate: f64,
    inv_sr: f32,
    filters: [Biquad; 2],
    envelopes: [f32; 2],
    lfo_phase: f32,
}

impl WahWah {
    pub fn new(params: Arc<WahWahParams>) -> Self {
        Self {
            params,
            sample_rate: 44_100.0,
            inv_sr: 1.0 / 44_100.0,
            filters: Default::default(),
            envelopes: [0.0; 2],
            lfo_phase: 0.0,
        }
    }
}

const MIN_HZ: f32 = 200.0;
const MAX_HZ: f32 = 1300.0;
const INV_E: f32 = 0.367_879_45;

fn attack_release_coeff(value_s: f32, inv_sr: f32) -> f32 {
    if value_s <= 0.0 {
        0.0
    } else {
        INV_E.powf(inv_sr / value_s)
    }
}

impl PluginLogic for WahWah {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        self.sample_rate = sample_rate;
        #[allow(clippy::cast_possible_truncation)]
        {
            self.inv_sr = 1.0 / sample_rate as f32;
        }
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        for f in &mut self.filters {
            f.reset();
        }
        self.envelopes = [0.0; 2];
        self.lfo_phase = 0.0;
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let total = buffer.num_samples();
        let mode = self.params.mode.value();
        let ty = self.params.filter_type.value();
        let num_ch = buffer.channels().min(self.filters.len());
        let automatic = matches!(mode, Mode::Automatic);

        let mut mix = [0.0_f32; MAX_BLOCK];
        let mut env_attack = [0.0_f32; MAX_BLOCK];
        let mut env_release = [0.0_f32; MAX_BLOCK];
        let mut lfo_rate = [0.0_f32; MAX_BLOCK];
        let mut lfo_env_mix = [0.0_f32; MAX_BLOCK];
        let mut manual_freq = [0.0_f32; MAX_BLOCK];
        let mut q_arr = [0.0_f32; MAX_BLOCK];
        let mut gain_arr = [0.0_f32; MAX_BLOCK];

        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(MAX_BLOCK);

            // Slice-based read advances each smoother by `n`. See
            // flanger for the rationale.
            self.params.mix.read_into(&mut mix[..n]);
            self.params.env_attack.read_into(&mut env_attack[..n]);
            self.params.env_release.read_into(&mut env_release[..n]);
            self.params.lfo_rate.read_into(&mut lfo_rate[..n]);
            self.params.lfo_env_mix.read_into(&mut lfo_env_mix[..n]);
            self.params.frequency.read_into(&mut manual_freq[..n]);
            self.params.q.read_into(&mut q_arr[..n]);
            self.params.gain.read_into(&mut gain_arr[..n]);

            // Pre-compute attack/release coefficients and the
            // shared LFO contribution so the inner channel-major
            // loop only does per-channel envelope + filter work.
            let mut attack_co = [0.0_f32; MAX_BLOCK];
            let mut release_co = [0.0_f32; MAX_BLOCK];
            let mut lfo_norm = [0.0_f32; MAX_BLOCK];
            for i in 0..n {
                attack_co[i] = attack_release_coeff(env_attack[i], self.inv_sr);
                release_co[i] = attack_release_coeff(env_release[i], self.inv_sr);
                if automatic {
                    lfo_norm[i] =
                        0.5 + 0.5 * (std::f32::consts::TAU * self.lfo_phase).sin();
                    self.lfo_phase += lfo_rate[i] * self.inv_sr;
                    if self.lfo_phase >= 1.0 {
                        self.lfo_phase -= 1.0;
                    }
                }
            }

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                let filter = &mut self.filters[ch];
                let mut env = self.envelopes[ch];
                for i in 0..n {
                    let idx = offset + i;
                    let in_sample = inp[idx];
                    let abs_in = in_sample.abs();
                    env = if abs_in > env {
                        attack_co[i] * env + (1.0 - attack_co[i]) * abs_in
                    } else {
                        release_co[i] * env + (1.0 - release_co[i]) * abs_in
                    };

                    let centre_freq_hz = if automatic {
                        let env_norm = env.clamp(0.0, 1.0);
                        let mixed = lfo_norm[i] + lfo_env_mix[i] * (env_norm - lfo_norm[i]);
                        MIN_HZ + mixed * (MAX_HZ - MIN_HZ)
                    } else {
                        manual_freq[i]
                    };
                    let q = f64::from(q_arr[i]);
                    let gain_lin = 10f64.powf(f64::from(gain_arr[i]) * 0.05);
                    update(filter, f64::from(centre_freq_hz), q, gain_lin, ty, self.sample_rate);

                    #[allow(clippy::cast_possible_truncation)]
                    let filtered = filter.process(f64::from(in_sample)) as f32;
                    out[idx] = in_sample + mix[i] * (filtered - in_sample);
                }
                self.envelopes[ch] = env;
            }

            offset += n;
        }

        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![
            section(
                "FILTER",
                vec![
                    dropdown(P::FilterType, "Type").cols(2),
                    knob(P::Frequency, "Freq"),
                    knob(P::Q, "Q"),
                    knob(P::Gain, "Gain"),
                    knob(P::Mix, "Mix"),
                ],
            ),
            section(
                "CONTROL",
                vec![
                    dropdown(P::Mode, "Mode").cols(2),
                    knob(P::LfoRate, "LFO"),
                    knob(P::LfoEnvMix, "LFO/Env"),
                    knob(P::EnvAttack, "Atk"),
                    knob(P::EnvRelease, "Rel"),
                ],
            ),
        ])
        .with_title("WAH-WAH")
        .with_cols(6)
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: WahWah,
    params: WahWahParams,
}
