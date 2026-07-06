//! Basic delay with feedback and mix using a circular delay line.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, knob, widgets};

use DelayParamsParamId as P;

const MAX_DELAY_SECS: f32 = 5.0;
const MAX_BLOCK: usize = 512;

#[derive(Params)]
pub struct DelayParams {
    #[param(
        name = "Delay Time",
        short_name = "Time",
        range = "linear(0.0, 5.0)",
        default = 0.1,
        unit = "s",
        smooth = "exp(5)"
    )]
    pub delay_time: FloatParam,

    #[param(
        name = "Feedback",
        range = "linear(0.0, 0.9)",
        default = 0.7,
        smooth = "exp(5)"
    )]
    pub feedback: FloatParam,

    #[param(
        name = "Mix",
        range = "linear(0.0, 1.0)",
        default = 1.0,
        smooth = "exp(5)"
    )]
    pub mix: FloatParam,
}

pub struct Delay {
    params: Arc<DelayParams>,
    sample_rate: f32,
    buffer: Vec<Vec<f32>>,
    buffer_len: usize,
    write_pos: usize,
}

impl Delay {
    pub fn new(params: Arc<DelayParams>) -> Self {
        Self {
            params,
            sample_rate: 44_100.0,
            buffer: Vec::new(),
            buffer_len: 1,
            write_pos: 0,
        }
    }
}

impl PluginLogic for Delay {
    type Params = DelayParams;

    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        let sr = sample_rate as f32;
        self.sample_rate = sr;
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let len = (MAX_DELAY_SECS * sr) as usize + 1;
        self.buffer_len = len.max(1);
        // Two channels - matches Reiss/McPherson layout. Mono input
        // just leaves the second lane silent.
        self.buffer = vec![vec![0.0; self.buffer_len]; 2];
        self.write_pos = 0;
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let total = buffer.num_samples();
        let buf_len = self.buffer_len;
        #[allow(clippy::cast_precision_loss)]
        let buf_len_f = buf_len as f32;
        let num_ch = buffer.channels().min(self.buffer.len());

        let mut delay_secs = [0.0_f32; MAX_BLOCK];
        let mut feedback = [0.0_f32; MAX_BLOCK];
        let mut mix = [0.0_f32; MAX_BLOCK];

        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(MAX_BLOCK);

            // Slice-based read advances each smoother by `n`. See
            // flanger for the rationale.
            self.params.delay_time.read_into(&mut delay_secs[..n]);
            self.params.feedback.read_into(&mut feedback[..n]);
            self.params.mix.read_into(&mut mix[..n]);

            // Precompute the read-position trajectory for the chunk
            // so the inner channel-major loop reads from stack
            // arrays only - no divides, no modulos.
            let mut read_idx = [0usize; MAX_BLOCK];
            let mut frac_arr = [0.0_f32; MAX_BLOCK];
            let mut write_idx = [0usize; MAX_BLOCK];
            let mut bypass = [false; MAX_BLOCK];
            for i in 0..n {
                let delay_samples = delay_secs[i] * self.sample_rate;
                let write = self.write_pos;
                #[allow(clippy::cast_precision_loss)]
                let read_pos = (write as f32 - delay_samples + buf_len_f).rem_euclid(buf_len_f);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let r0 = read_pos.floor() as usize;
                read_idx[i] = r0;
                frac_arr[i] = read_pos - read_pos.floor();
                write_idx[i] = write;
                bypass[i] = r0 == write;
                self.write_pos += 1;
                if self.write_pos >= buf_len {
                    self.write_pos -= buf_len;
                }
            }

            for ch in 0..num_ch {
                let (inp, out) = buffer.io(ch);
                let line = &mut self.buffer[ch];
                for i in 0..n {
                    let idx = offset + i;
                    let in_sample = inp[idx];
                    let write = write_idx[i];
                    if bypass[i] {
                        out[idx] = in_sample;
                        line[write] = in_sample;
                    } else {
                        let r0 = read_idx[i];
                        let d0 = line[r0];
                        let d1 = line[(r0 + 1) % buf_len];
                        let delayed = d0 + frac_arr[i] * (d1 - d0);
                        out[idx] = in_sample + mix[i] * (delayed - in_sample);
                        line[write] = in_sample + delayed * feedback[i];
                    }
                }
            }

            offset += n;
        }

        ProcessStatus::Normal
    }

    fn editor(params: Arc<DelayParams>) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            knob(P::DelayTime, "Time"),
            knob(P::Feedback, "Feedback"),
            knob(P::Mix, "Mix"),
        ])])
        .with_title("DELAY")
        .into_editor(&params)
    }
}

truce::plugin! {
    logic: Delay,
    params: DelayParams,
}
