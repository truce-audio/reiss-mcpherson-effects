//! Pitch Shift - real-time phase-vocoder pitch shifter.
//!
//! Uses [`realfft`] for the FFT and [`apodize`] for Hann / Hamming
//! windows. FFT size, hop ratio and window type are all live
//! parameters; switching them rebuilds the plans on the next
//! audio block (allocates on the audio thread - acceptable for a
//! teaching port).

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, dropdown, knob, widgets};

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use PitchShiftParamsParamId as P;

#[derive(ParamEnum)]
pub enum FftSize {
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
    Bartlett,
    Hann,
    Hamming,
}

#[derive(Params)]
pub struct PitchShiftParams {
    #[param(
        name = "Shift",
        range = "linear(-12.0, 12.0)",
        default = 0.0,
        unit = "st",
        smooth = "exp(5)"
    )]
    pub shift: FloatParam,

    #[param(name = "FFT Size", short_name = "FFT", default = 1)]
    pub fft_size: EnumParam<FftSize>,

    #[param(name = "Hop", default = 2)]
    pub hop: EnumParam<HopRatio>,

    #[param(name = "Window", default = 1)]
    pub window: EnumParam<WindowType>,
}

fn build_window(window: WindowType, len: usize, out: &mut Vec<f32>) {
    out.clear();
    out.reserve(len);
    match window {
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

/// Wrap `phase` to `[-PI, PI]`. The phase-vocoder needs this every
/// time it accumulates a deviation, otherwise rounding error spirals.
fn princ_arg(phase: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let pi = std::f32::consts::PI;
    if phase >= 0.0 {
        ((phase + pi) % tau) - pi
    } else {
        ((phase + pi) % -tau) + pi
    }
}

struct PhaseVocoder {
    fft_size: usize,
    overlap: usize,
    hop_size: usize,
    window_type: WindowType,

    /// Analysis window (sqrt applied per-sample in the inner loop -
    /// keeps the synthesis window symmetric with the analysis one
    /// even when the resampled length differs).
    fft_window: Vec<f32>,
    window_scale: f32,

    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    freq: Vec<Complex<f32>>,

    /// Output ring is sized to fit the worst-case (lowest ratio,
    /// longest resampled IFFT). At a -12 semitone shift ratio is
    /// 0.5, so the resampled output is 2x the FFT size.
    output_len: usize,
    input_buffer: Vec<Vec<f32>>,
    output_buffer: Vec<Vec<f32>>,

    /// Per-channel, per-bin running phase from the previous hop -
    /// stores the *measured* input phase to compute the next
    /// deviation against.
    input_phase: Vec<Vec<f32>>,
    /// Per-channel, per-bin synthesised output phase - what we put
    /// back on the bin after scaling by `ratio`.
    output_phase: Vec<Vec<f32>>,

    /// `omega[k] = 2*pi*k / fft_size`, the expected per-hop phase
    /// advance of bin k. Precomputed.
    omega: Vec<f32>,

    input_write_pos: usize,
    output_write_pos: usize,
    output_read_pos: usize,
    samples_since_last_fft: usize,
    /// Set when shift is mid-smoothing so we know to flush the
    /// phase accumulators when the target finally arrives.
    need_phase_reset: bool,

    /// Resampling / synthesis-window scratch. Length grows with
    /// `1 / ratio` (up to ~2x fft_size at -12 semitones).
    resampled: Vec<f32>,
    synthesis_window: Vec<f32>,
}

impl PhaseVocoder {
    fn new(planner: &mut RealFftPlanner<f32>, num_channels: usize) -> Self {
        let fft_size = 512;
        let overlap = 8;
        let fwd = planner.plan_fft_forward(fft_size);
        let inv = planner.plan_fft_inverse(fft_size);
        let mut fft_window = Vec::new();
        build_window(WindowType::Hann, fft_size, &mut fft_window);
        let scale = window_scale(&fft_window, overlap);
        let hop = fft_size / overlap;
        // 2x is the worst-case stretch (shift = -12 st → ratio = 0.5).
        let output_len = fft_size * 2;
        let num_bins = fft_size / 2 + 1;
        Self {
            fft_size,
            overlap,
            hop_size: hop,
            window_type: WindowType::Hann,
            fft_window,
            window_scale: scale,
            fwd,
            inv,
            time: vec![0.0; fft_size],
            freq: vec![Complex::new(0.0, 0.0); num_bins],
            output_len,
            input_buffer: vec![vec![0.0; fft_size]; num_channels.max(1)],
            output_buffer: vec![vec![0.0; output_len]; num_channels.max(1)],
            input_phase: vec![vec![0.0; num_bins]; num_channels.max(1)],
            output_phase: vec![vec![0.0; num_bins]; num_channels.max(1)],
            omega: {
                let mut v = vec![0.0; num_bins];
                #[allow(clippy::cast_precision_loss)]
                let n = fft_size as f32;
                for (k, slot) in v.iter_mut().enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let kk = k as f32;
                    *slot = std::f32::consts::TAU * kk / n;
                }
                v
            },
            input_write_pos: 0,
            output_write_pos: hop,
            output_read_pos: 0,
            samples_since_last_fft: 0,
            need_phase_reset: true,
            resampled: vec![0.0; output_len],
            synthesis_window: vec![0.0; output_len],
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
            let num_bins = fft_size / 2 + 1;
            self.freq = vec![Complex::new(0.0, 0.0); num_bins];
            self.input_buffer = vec![vec![0.0; fft_size]; num_channels.max(1)];
            self.output_len = fft_size * 2;
            self.output_buffer = vec![vec![0.0; self.output_len]; num_channels.max(1)];
            self.input_phase = vec![vec![0.0; num_bins]; num_channels.max(1)];
            self.output_phase = vec![vec![0.0; num_bins]; num_channels.max(1)];
            self.omega = {
                let mut v = vec![0.0; num_bins];
                #[allow(clippy::cast_precision_loss)]
                let nf = fft_size as f32;
                for (k, slot) in v.iter_mut().enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let kk = k as f32;
                    *slot = std::f32::consts::TAU * kk / nf;
                }
                v
            };
            self.resampled = vec![0.0; self.output_len];
            self.synthesis_window = vec![0.0; self.output_len];
            self.input_write_pos = 0;
            self.output_read_pos = 0;
            self.samples_since_last_fft = 0;
            self.need_phase_reset = true;
        }
        if size_changed
            || overlap != self.overlap
            || window_type as u8 != self.window_type as u8
        {
            build_window(window_type, fft_size, &mut self.fft_window);
            self.window_scale = window_scale(&self.fft_window, overlap);
            self.hop_size = fft_size / overlap.max(1);
            self.output_write_pos = self.hop_size.min(self.output_len.max(1) - 1);
        }
        self.fft_size = fft_size;
        self.overlap = overlap;
        self.window_type = window_type;
    }
}

pub struct PitchShift {
    params: Arc<PitchShiftParams>,
    planner: RealFftPlanner<f32>,
    pv: Option<PhaseVocoder>,
    num_channels: usize,
}

impl PitchShift {
    pub fn new(params: Arc<PitchShiftParams>) -> Self {
        Self {
            params,
            planner: RealFftPlanner::<f32>::new(),
            pv: None,
            num_channels: 2,
        }
    }
}

impl PluginLogic for PitchShift {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();
        self.pv = Some(PhaseVocoder::new(&mut self.planner, self.num_channels));
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let channels = buffer.channels();
        self.num_channels = channels.max(1);
        let target_size = self.params.fft_size.value().samples();
        let target_overlap = self.params.hop.value().overlap();
        let target_window = self.params.window.value();

        let pv = self
            .pv
            .get_or_insert_with(|| PhaseVocoder::new(&mut self.planner, self.num_channels));
        pv.reconfigure(
            &mut self.planner,
            target_size,
            target_overlap,
            target_window,
            self.num_channels,
        );

        let n = buffer.num_samples();
        let fft_size = pv.fft_size;
        let hop_size = pv.hop_size.max(1);
        #[allow(clippy::cast_precision_loss)]
        let hop_f = hop_size as f32;

        // Snap the shift factor to integer-hop ratio. The book's
        // recipe — guarantees the per-bin phase increment is an
        // integer multiple of the FFT bin spacing, eliminating
        // long-running phase drift between successive hops.
        let shift_st = self.params.shift.read();
        let raw_factor = 2f32.powf(shift_st / 12.0);
        let ratio = (raw_factor * hop_f).round() / hop_f;
        let ratio = ratio.max(1e-3);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let resampled_len = ((fft_size as f32) / ratio).floor() as usize;
        let resampled_len = resampled_len.min(pv.output_len);

        // Rebuild synthesis window each block - cheap, lets the
        // resampled length track the shift smoothly.
        build_window(pv.window_type, resampled_len, &mut pv.synthesis_window);

        let saved_in = pv.input_write_pos;
        let saved_out_r = pv.output_read_pos;
        let saved_out_w = pv.output_write_pos;
        let saved_count = pv.samples_since_last_fft;
        let saved_reset = pv.need_phase_reset;

        for ch in 0..channels {
            pv.input_write_pos = saved_in;
            pv.output_read_pos = saved_out_r;
            pv.output_write_pos = saved_out_w;
            pv.samples_since_last_fft = saved_count;
            pv.need_phase_reset = saved_reset;

            for i in 0..n {
                let in_sample = buffer.io(ch).0[i];

                // Read output first so a zero-latency pass-through
                // is possible (output starts pre-filled with zeros).
                let out_sample = pv.output_buffer[ch][pv.output_read_pos];
                pv.output_buffer[ch][pv.output_read_pos] = 0.0;
                pv.output_read_pos += 1;
                if pv.output_read_pos >= pv.output_len {
                    pv.output_read_pos = 0;
                }
                buffer.io(ch).1[i] = out_sample;

                pv.input_buffer[ch][pv.input_write_pos] = in_sample;
                pv.input_write_pos += 1;
                if pv.input_write_pos >= fft_size {
                    pv.input_write_pos = 0;
                }

                pv.samples_since_last_fft += 1;
                if pv.samples_since_last_fft >= hop_size {
                    pv.samples_since_last_fft = 0;
                    do_hop(pv, ch, ratio, resampled_len);
                    pv.output_write_pos += hop_size;
                    if pv.output_write_pos >= pv.output_len {
                        pv.output_write_pos -= pv.output_len;
                    }
                }
            }
        }

        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            knob(P::Shift, "Shift"),
            dropdown(P::FftSize, "FFT"),
            dropdown(P::Hop, "Hop"),
            dropdown(P::Window, "Window"),
        ])])
        .with_title("PITCH SHIFT")
        .into_editor(&self.params)
    }
}

#[allow(clippy::too_many_lines)]
fn do_hop(pv: &mut PhaseVocoder, ch: usize, ratio: f32, resampled_len: usize) {
    let fft_size = pv.fft_size;
    let num_bins = fft_size / 2 + 1;
    let hop_size = pv.hop_size;
    #[allow(clippy::cast_precision_loss)]
    let hop_f = hop_size as f32;

    // Analysis: pull fft_size samples out of the input ring, apply
    // sqrt(window). Sqrt because the synthesis path also applies
    // sqrt(window) to the resampled output — together they reproduce
    // the unit-overlap window energy.
    let buf = &pv.input_buffer[ch];
    let mut idx = pv.input_write_pos;
    for i in 0..fft_size {
        pv.time[i] = pv.fft_window[i].sqrt() * buf[idx];
        idx += 1;
        if idx >= fft_size {
            idx = 0;
        }
    }

    let _ = pv.fwd.process(&mut pv.time, &mut pv.freq);

    if pv.need_phase_reset {
        for v in &mut pv.input_phase[ch] {
            *v = 0.0;
        }
        for v in &mut pv.output_phase[ch] {
            *v = 0.0;
        }
        pv.need_phase_reset = false;
    }

    for k in 0..num_bins {
        let c = pv.freq[k];
        let magnitude = (c.re * c.re + c.im * c.im).sqrt();
        let phase = c.im.atan2(c.re);

        let phase_dev = phase - pv.input_phase[ch][k] - pv.omega[k] * hop_f;
        let delta_phi = pv.omega[k] * hop_f + princ_arg(phase_dev);
        let new_phase = princ_arg(pv.output_phase[ch][k] + delta_phi * ratio);

        pv.input_phase[ch][k] = phase;
        pv.output_phase[ch][k] = new_phase;
        let (s, co) = new_phase.sin_cos();
        pv.freq[k] = Complex::new(magnitude * co, magnitude * s);
    }

    let _ = pv.inv.process(&mut pv.freq, &mut pv.time);

    // Resample the IFFT output by `1 / ratio` via linear interp;
    // this is what actually shifts the pitch in the time domain.
    #[allow(clippy::cast_precision_loss)]
    let resampled_len_f = resampled_len as f32;
    #[allow(clippy::cast_precision_loss)]
    let fft_size_f = fft_size as f32;
    let inv_n = 1.0 / fft_size_f;
    for j in 0..resampled_len {
        #[allow(clippy::cast_precision_loss)]
        let jf = j as f32;
        let x = jf * fft_size_f / resampled_len_f;
        let ix = x.floor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i_low = ix as usize % fft_size;
        let dx = x - ix;
        let s0 = pv.time[i_low];
        let s1 = pv.time[(i_low + 1) % fft_size];
        // C2R is unnormalised; the 1/N here closes the round trip.
        pv.resampled[j] = (s0 + dx * (s1 - s0)) * inv_n * pv.synthesis_window[j].sqrt();
    }

    // Overlap-add the windowed resampled signal into the output
    // ring, starting at `output_write_pos`.
    let out = &mut pv.output_buffer[ch];
    let out_len = out.len();
    let mut idx = pv.output_write_pos;
    for j in 0..resampled_len {
        out[idx] += pv.resampled[j] * pv.window_scale;
        idx += 1;
        if idx >= out_len {
            idx = 0;
        }
    }
}

truce::plugin! {
    logic: PitchShift,
    params: PitchShiftParams,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn princ_arg_wraps() {
        let two_pi = std::f32::consts::TAU;
        for x in [0.0_f32, 0.5, 1.0, 3.0, 4.0, -3.5, 7.0] {
            let w = princ_arg(x);
            assert!(w >= -std::f32::consts::PI - 1e-5);
            assert!(w <= std::f32::consts::PI + 1e-5);
            // Wrapped value must differ from input by a multiple of 2π.
            let diff = (x - w) / two_pi;
            assert!((diff - diff.round()).abs() < 1e-3);
        }
    }
}
