//! Robotization / Whisperization - phase-vocoder STFT effect that
//! either zeroes the per-bin phase (robotization → constant-pitch
//! voice) or randomises it (whisperization → noisy unpitched voice).
//!
//! FFT size, hop-ratio and window type are all switchable at
//! runtime — when the host changes one, `process` rebuilds the
//! plans on the next block. That allocates on the audio thread,
//! which is fine for a teaching port.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, widgets};

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use RobotParamsParamId as P;

#[derive(ParamEnum)]
pub enum Effect {
    #[name = "Pass-Through"]
    PassThrough,
    Robotization,
    Whisperization,
}

#[derive(ParamEnum)]
pub enum FftSize {
    #[name = "32"]
    S32,
    #[name = "64"]
    S64,
    #[name = "128"]
    S128,
    #[name = "256"]
    S256,
    #[name = "512"]
    S512,
    #[name = "1024"]
    S1024,
    #[name = "2048"]
    S2048,
    #[name = "4096"]
    S4096,
}

impl FftSize {
    fn samples(self) -> usize {
        match self {
            FftSize::S32 => 32,
            FftSize::S64 => 64,
            FftSize::S128 => 128,
            FftSize::S256 => 256,
            FftSize::S512 => 512,
            FftSize::S1024 => 1024,
            FftSize::S2048 => 2048,
            FftSize::S4096 => 4096,
        }
    }
}

#[derive(ParamEnum)]
pub enum HopRatio {
    #[name = "1/2"]
    Half,
    #[name = "1/4"]
    Quarter,
    #[name = "1/8"]
    Eighth,
}

impl HopRatio {
    fn overlap(self) -> usize {
        match self {
            HopRatio::Half => 2,
            HopRatio::Quarter => 4,
            HopRatio::Eighth => 8,
        }
    }
}

#[derive(ParamEnum)]
pub enum WindowType {
    Rectangular,
    Bartlett,
    Hann,
    Hamming,
}

#[derive(Params)]
pub struct RobotParams {
    #[param(name = "Effect", default = 1)]
    pub effect: EnumParam<Effect>,

    /// Powers of two: 32, 64, …, 4096. Sets the analysis block
    /// length and therefore the time / frequency trade-off.
    #[param(name = "FFT Size", short_name = "FFT", default = 4)]
    pub fft_size: EnumParam<FftSize>,

    /// Hop = FFT / overlap. Smaller overlap (1/2) is cheaper but
    /// more audibly granular; 1/8 sounds smoothest.
    #[param(name = "Hop", default = 2)]
    pub hop: EnumParam<HopRatio>,

    #[param(name = "Window", default = 2)]
    pub window: EnumParam<WindowType>,
}

/// Build a window of `len` samples for the chosen type. Uses
/// `apodize` for the smooth windows; the trivial ones are inline
/// because there's nothing to delegate.
fn build_window(window: WindowType, len: usize, out: &mut Vec<f32>) {
    out.clear();
    out.reserve(len);
    match window {
        WindowType::Rectangular => out.extend(std::iter::repeat_n(1.0, len)),
        WindowType::Bartlett => {
            #[allow(clippy::cast_precision_loss)]
            let denom = (len - 1) as f32;
            for i in 0..len {
                #[allow(clippy::cast_precision_loss)]
                let n = i as f32;
                out.push(1.0 - (2.0 * n / denom - 1.0).abs());
            }
        }
        WindowType::Hann => {
            #[allow(clippy::cast_possible_truncation)]
            out.extend(apodize::hanning_iter(len).map(|v| v as f32));
        }
        WindowType::Hamming => {
            #[allow(clippy::cast_possible_truncation)]
            out.extend(apodize::hamming_iter(len).map(|v| v as f32));
        }
    }
}

/// Synthesis gain - book formula: `fft_size / overlap / sum(window)`.
fn window_scale(window: &[f32], overlap: usize) -> f32 {
    let sum: f32 = window.iter().copied().sum();
    if overlap == 0 || sum == 0.0 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        let n = window.len() as f32;
        #[allow(clippy::cast_precision_loss)]
        let o = overlap as f32;
        n / o / sum
    }
}

struct Stft {
    fft_size: usize,
    overlap: usize,
    hop_size: usize,
    window_type: WindowType,
    window: Vec<f32>,
    window_scale: f32,

    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    freq: Vec<Complex<f32>>,

    input_buffer: Vec<Vec<f32>>,
    output_buffer: Vec<Vec<f32>>,

    input_write_pos: usize,
    output_write_pos: usize,
    output_read_pos: usize,
    samples_since_last_fft: usize,
}

impl Stft {
    fn new(planner: &mut RealFftPlanner<f32>, num_channels: usize) -> Self {
        let fft_size = 512;
        let overlap = 8;
        let fwd = planner.plan_fft_forward(fft_size);
        let inv = planner.plan_fft_inverse(fft_size);
        let mut window = Vec::new();
        build_window(WindowType::Hann, fft_size, &mut window);
        let scale = window_scale(&window, overlap);
        let hop = fft_size / overlap;
        Self {
            fft_size,
            overlap,
            hop_size: hop,
            window_type: WindowType::Hann,
            window,
            window_scale: scale,
            fwd,
            inv,
            time: vec![0.0; fft_size],
            freq: vec![Complex::new(0.0, 0.0); fft_size / 2 + 1],
            input_buffer: vec![vec![0.0; fft_size]; num_channels.max(1)],
            output_buffer: vec![vec![0.0; fft_size]; num_channels.max(1)],
            input_write_pos: 0,
            output_write_pos: hop,
            output_read_pos: 0,
            samples_since_last_fft: 0,
        }
    }

    fn reconfigure(
        &mut self,
        planner: &mut RealFftPlanner<f32>,
        fft_size: usize,
        overlap: usize,
        window_type: WindowType,
        num_channels: usize,
    ) {
        let size_changed = fft_size != self.fft_size;
        if size_changed {
            self.fwd = planner.plan_fft_forward(fft_size);
            self.inv = planner.plan_fft_inverse(fft_size);
            self.time = vec![0.0; fft_size];
            self.freq = vec![Complex::new(0.0, 0.0); fft_size / 2 + 1];
            self.input_buffer = vec![vec![0.0; fft_size]; num_channels.max(1)];
            self.output_buffer = vec![vec![0.0; fft_size]; num_channels.max(1)];
            self.input_write_pos = 0;
            self.output_read_pos = 0;
            self.samples_since_last_fft = 0;
        }
        if size_changed
            || overlap != self.overlap
            || matches!(window_type, _ if window_type as u8 != self.window_type as u8)
        {
            build_window(window_type, fft_size, &mut self.window);
            self.window_scale = window_scale(&self.window, overlap);
            self.hop_size = fft_size / overlap.max(1);
            self.output_write_pos = self.hop_size.min(fft_size.max(1) - 1);
        }
        self.fft_size = fft_size;
        self.overlap = overlap;
        self.window_type = window_type;
    }

    /// Pull `fft_size` samples from the input ring (starting at the
    /// channel's current write head — i.e. oldest first) into the
    /// time-domain scratch with the analysis window applied.
    fn analysis(&mut self, channel: usize) {
        let n = self.fft_size;
        let buf = &self.input_buffer[channel];
        let len = buf.len();
        let mut idx = self.input_write_pos;
        for i in 0..n {
            self.time[i] = self.window[i] * buf[idx];
            idx += 1;
            if idx >= len {
                idx = 0;
            }
        }
    }

    /// Window-and-overlap-add the time-domain scratch onto the
    /// output ring, starting at `output_write_pos`.
    fn synthesis(&mut self, channel: usize) {
        let n = self.fft_size;
        let buf = &mut self.output_buffer[channel];
        let len = buf.len();
        let mut idx = self.output_write_pos;
        #[allow(clippy::cast_precision_loss)]
        let inv_n = 1.0 / n as f32;
        for i in 0..n {
            // realfft's C2R is unnormalised - divide by N here so
            // the round trip is the identity (window_scale already
            // accounts for overlap).
            buf[idx] += self.time[i] * self.window_scale * inv_n;
            idx += 1;
            if idx >= len {
                idx = 0;
            }
        }
    }
}

pub struct Robotization;

pub struct RobotizationDsp {
    planner: RealFftPlanner<f32>,
    stft: Option<Stft>,
    num_channels: usize,
    rng: fastrand::Rng,
}

impl Default for RobotizationDsp {
    fn default() -> Self {
        Self {
            planner: RealFftPlanner::<f32>::new(),
            stft: None,
            num_channels: 2,
            rng: fastrand::Rng::new(),
        }
    }
}

impl PluginLogic for Robotization {
    type Params = RobotParams;
    type DspState = RobotizationDsp;

    fn reset(state: &mut Self::DspState, _params: &Self::Params, _config: &AudioConfig) {
        state.stft = Some(Stft::new(&mut state.planner, state.num_channels));
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let channels = buffer.channels();
        state.num_channels = channels.max(1);

        let target_size = params.fft_size.value().samples();
        let target_overlap = params.hop.value().overlap();
        let target_window = params.window.value();
        let effect = params.effect.value();

        let stft = state
            .stft
            .get_or_insert_with(|| Stft::new(&mut state.planner, state.num_channels));
        stft.reconfigure(
            &mut state.planner,
            target_size,
            target_overlap,
            target_window,
            state.num_channels,
        );

        let n = buffer.num_samples();
        let fft_size = stft.fft_size;
        let hop_size = stft.hop_size.max(1);

        // The framework's `for_each_frame` can't help here - the
        // STFT state is shared across channels but the inner FFT
        // runs per-channel. Loop channels outside, samples inside,
        // and stash positions per channel.
        let saved_in = stft.input_write_pos;
        let saved_out_r = stft.output_read_pos;
        let saved_out_w = stft.output_write_pos;
        let saved_count = stft.samples_since_last_fft;

        for ch in 0..channels {
            stft.input_write_pos = saved_in;
            stft.output_read_pos = saved_out_r;
            stft.output_write_pos = saved_out_w;
            stft.samples_since_last_fft = saved_count;

            for i in 0..n {
                let in_sample = buffer.io(ch).0[i];

                // Write input ring.
                stft.input_buffer[ch][stft.input_write_pos] = in_sample;
                stft.input_write_pos += 1;
                if stft.input_write_pos >= fft_size {
                    stft.input_write_pos = 0;
                }

                // Read output ring (and clear after).
                let out_sample = stft.output_buffer[ch][stft.output_read_pos];
                stft.output_buffer[ch][stft.output_read_pos] = 0.0;
                stft.output_read_pos += 1;
                if stft.output_read_pos >= fft_size {
                    stft.output_read_pos = 0;
                }
                buffer.io(ch).1[i] = out_sample;

                stft.samples_since_last_fft += 1;
                if stft.samples_since_last_fft >= hop_size {
                    stft.samples_since_last_fft = 0;

                    stft.analysis(ch);
                    let _ = stft.fwd.process(&mut stft.time, &mut stft.freq);
                    modify(effect, &mut stft.freq, fft_size, &mut state.rng);
                    let _ = stft.inv.process(&mut stft.freq, &mut stft.time);
                    stft.synthesis(ch);

                    stft.output_write_pos += hop_size;
                    if stft.output_write_pos >= fft_size {
                        stft.output_write_pos -= fft_size;
                    }
                }
            }
        }

        ProcessStatus::Normal
    }

    fn editor(params: Arc<RobotParams>) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            dropdown(P::Effect, "Effect").cols(2),
            dropdown(P::FftSize, "FFT"),
            dropdown(P::Hop, "Hop"),
            dropdown(P::Window, "Window").cols(2),
        ])])
        .with_title("ROBOTIZATION")
        .with_cols(6)
        .into_editor(&params)
    }
}

fn modify(effect: Effect, freq: &mut [Complex<f32>], fft_size: usize, rng: &mut fastrand::Rng) {
    match effect {
        Effect::PassThrough => {}
        Effect::Robotization => {
            // Zero the phase of every bin → output spectrum is real
            // and (still) conjugate-symmetric. Bins 0 and N/2 keep
            // their sign; intermediate bins use |X[k]|.
            for c in freq.iter_mut() {
                let mag = (c.re * c.re + c.im * c.im).sqrt();
                c.re = mag;
                c.im = 0.0;
            }
        }
        Effect::Whisperization => {
            // Random per-bin phase, conjugate symmetry preserved by
            // realfft because we only write bins 0..N/2 and the
            // inverse mirrors. DC and Nyquist bins must stay real -
            // any imaginary residue there leaks into the inverse.
            let nyquist = fft_size / 2;
            for (k, c) in freq.iter_mut().enumerate() {
                let mag = (c.re * c.re + c.im * c.im).sqrt();
                if k == 0 || k == nyquist {
                    c.re = mag;
                    c.im = 0.0;
                } else {
                    let phase = std::f32::consts::TAU * rng.f32();
                    let (s, co) = phase.sin_cos();
                    c.re = mag * co;
                    c.im = mag * s;
                }
            }
        }
    }
}

truce::plugin! {
    logic: Robotization,
    params: RobotParams,
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
