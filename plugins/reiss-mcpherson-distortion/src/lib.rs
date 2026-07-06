//! Distortion - waveshaper plus a tone-control high-shelf.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, widgets};

use DistortionParamsParamId as P;

const MAX_BLOCK: usize = 512;

#[derive(ParamEnum)]
pub enum DistortionType {
    #[name = "Hard Clip"]
    HardClipping,
    #[name = "Soft Clip"]
    SoftClipping,
    Exponential,
    #[name = "Full Rect"]
    FullWaveRectifier,
    #[name = "Half Rect"]
    HalfWaveRectifier,
}

#[derive(Params)]
pub struct DistortionParams {
    #[param(name = "Type", default = 3)]
    pub distortion_type: EnumParam<DistortionType>,

    #[param(
        name = "Input Gain",
        short_name = "In",
        range = "linear(-24.0, 24.0)",
        default = 12.0,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub input_gain: FloatParam,

    #[param(
        name = "Output Gain",
        short_name = "Out",
        range = "linear(-24.0, 24.0)",
        default = -24.0,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub output_gain: FloatParam,

    #[param(
        name = "Tone",
        range = "linear(-24.0, 24.0)",
        default = 12.0,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub tone: FloatParam,
}

#[derive(Default, Clone, Copy)]
struct ToneShelf {
    b0: f32,
    b1: f32,
    a1: f32,
    x1: f32,
    y1: f32,
}

impl ToneShelf {
    fn update(&mut self, tone_db: f32) {
        // The book pins the discrete cut-off at PI * 0.01 and only
        // moves the shelf gain. Keeps the tone control "flat at
        // unity" behaviour intuitive.
        let discrete = std::f32::consts::PI * 0.01;
        let gain_lin = 10f32.powf(tone_db * 0.05);
        let tan_half = (discrete * 0.5).tan();
        let sqrt_g = gain_lin.sqrt();
        let a0 = sqrt_g * tan_half + 1.0;
        let inv = 1.0 / a0;
        self.b0 = (sqrt_g * tan_half + gain_lin) * inv;
        self.b1 = (sqrt_g * tan_half - gain_lin) * inv;
        self.a1 = (sqrt_g * tan_half - 1.0) * inv;
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

fn shape(x: f32, ty: DistortionType) -> f32 {
    match ty {
        DistortionType::HardClipping => {
            const T: f32 = 0.5;
            x.clamp(-T, T)
        }
        DistortionType::SoftClipping => {
            // Two-sided cubic-style soft knee from Reiss/McPherson:
            // linear in [-1/3, 1/3], parabolic in the next third, and
            // hard saturation past 2/3. Output renormalised to [-1, 1]
            // then halved.
            let t1 = 1.0 / 3.0;
            let t2 = 2.0 / 3.0;
            let raw = if x > t2 {
                1.0
            } else if x > t1 {
                let inner = 2.0 - 3.0 * x;
                1.0 - (inner * inner) / 3.0
            } else if x < -t2 {
                -1.0
            } else if x < -t1 {
                let inner = 2.0 + 3.0 * x;
                -1.0 + (inner * inner) / 3.0
            } else {
                2.0 * x
            };
            raw * 0.5
        }
        DistortionType::Exponential => {
            if x > 0.0 {
                1.0 - (-x).exp()
            } else {
                -1.0 + x.exp()
            }
        }
        DistortionType::FullWaveRectifier => x.abs(),
        DistortionType::HalfWaveRectifier => x.max(0.0),
    }
}

pub struct Distortion {
    params: Arc<DistortionParams>,
    shelves: [ToneShelf; 2],
}

impl Distortion {
    pub fn new(params: Arc<DistortionParams>) -> Self {
        Self {
            params,
            shelves: [ToneShelf::default(); 2],
        }
    }
}

impl PluginLogic for Distortion {
    type Params = DistortionParams;

    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        for s in &mut self.shelves {
            s.reset();
        }
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let total = buffer.num_samples();
        let ty = self.params.distortion_type.value();
        let num_ch = buffer.channels().min(self.shelves.len());

        let mut in_gain = [0.0_f32; MAX_BLOCK];
        let mut out_gain = [0.0_f32; MAX_BLOCK];
        let mut in_lin = [0.0_f32; MAX_BLOCK];
        let mut out_lin = [0.0_f32; MAX_BLOCK];

        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(MAX_BLOCK);

            // Tone shelf rebuilds once per chunk - recomputing at
            // sample rate buys nothing audible at PI*0.01 cut-off
            // and we still need the smoother advanced by `n`.
            let tone = self.params.tone.read_after(n);
            for s in &mut self.shelves {
                s.update(tone);
            }

            self.params.input_gain.read_into(&mut in_gain[..n]);
            self.params.output_gain.read_into(&mut out_gain[..n]);
            for i in 0..n {
                in_lin[i] = 10f32.powf(in_gain[i] * 0.05);
                out_lin[i] = 10f32.powf(out_gain[i] * 0.05);
            }

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                let s = &mut self.shelves[ch];
                for i in 0..n {
                    let idx = offset + i;
                    let shaped = shape(inp[idx] * in_lin[i], ty);
                    let filtered = s.process(shaped);
                    out[idx] = filtered * out_lin[i];
                }
            }

            offset += n;
        }
        ProcessStatus::Normal
    }

    fn editor(params: Arc<DistortionParams>) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            dropdown(P::DistortionType, "Type").cols(2),
            knob(P::InputGain, "In"),
            knob(P::OutputGain, "Out"),
            knob(P::Tone, "Tone"),
        ])])
        .with_title("DISTORTION")
        .with_cols(5)
        .into_editor(&params)
    }
}

truce::plugin! {
    logic: Distortion,
    params: DistortionParams,
}
