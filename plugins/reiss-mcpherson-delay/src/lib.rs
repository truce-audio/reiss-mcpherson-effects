//! Basic delay with feedback and mix using a circular delay line.
//!
//! Ported from Juan Gil's "Audio Effects" Delay (chapter from Reiss /
//! McPherson). Fractional read positions are linearly interpolated.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, knob, widgets};

use DelayParamsParamId as P;

const MAX_DELAY_SECS: f32 = 5.0;

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
        let n = buffer.num_samples();
        let buf_len = self.buffer_len;
        #[allow(clippy::cast_precision_loss)]
        let buf_len_f = buf_len as f32;

        for ch in 0..buffer.channels().min(self.buffer.len()) {
            let mut write = self.write_pos;
            let (inp, out) = buffer.io(ch);
            let line = &mut self.buffer[ch];

            for i in 0..n {
                let delay_secs = self.params.delay_time.read();
                let feedback = self.params.feedback.read();
                let mix = self.params.mix.read();
                let delay_samples = delay_secs * self.sample_rate;

                #[allow(clippy::cast_precision_loss)]
                let read_pos = (write as f32 - delay_samples + buf_len_f).rem_euclid(buf_len_f);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let read_idx = read_pos.floor() as usize;

                let in_sample = inp[i];
                if read_idx != write {
                    let frac = read_pos - read_pos.floor();
                    let d0 = line[read_idx];
                    let d1 = line[(read_idx + 1) % buf_len];
                    let delayed = d0 + frac * (d1 - d0);

                    out[i] = in_sample + mix * (delayed - in_sample);
                    line[write] = in_sample + delayed * feedback;
                } else {
                    out[i] = in_sample;
                    line[write] = in_sample;
                }

                write += 1;
                if write >= buf_len {
                    write -= buf_len;
                }
            }

            // Last channel processed writes back the canonical
            // position so all channels share the same write head.
            self.write_pos = write;
        }

        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            knob(P::DelayTime, "Time"),
            knob(P::Feedback, "Feedback"),
            knob(P::Mix, "Mix"),
        ])])
        .with_title("DELAY")
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: Delay,
    params: DelayParams,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_is_valid() {
        truce_test::assert_valid_info::<Plugin>();
    }

    #[test]
    fn has_editor() {
        truce_test::assert_has_editor::<Plugin>();
    }

    #[test]
    fn state_round_trips() {
        truce_test::assert_state_round_trip::<Plugin>();
    }

    #[test]
    fn renders_no_nans() {
        use std::time::Duration;
        use truce_test::{InputSource, assertions, driver};
        let result = driver!(Plugin)
            .duration(Duration::from_millis(50))
            .input(InputSource::Constant(0.5))
            .run();
        assertions::assert_no_nans(&result);
    }
}
