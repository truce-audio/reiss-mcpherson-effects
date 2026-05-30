//! Ping-Pong Delay - stereo delay where the feedback path crosses
//! channels so each repeat bounces L <-> R.
//!
//! Ported from Juan Gil's "Audio Effects" Ping-Pong Delay.

use std::sync::Arc;
use truce::prelude::*;
use truce_gui::IntoLayoutEditor;
use truce_gui_types::layout::{GridLayout, knob, widgets};

use PingPongParamsParamId as P;

const MAX_DELAY_SECS: f32 = 5.0;

#[derive(Params)]
pub struct PingPongParams {
    /// 0.0 → only the L input feeds the delay, 1.0 → only the R input.
    #[param(
        name = "Balance",
        range = "linear(0.0, 1.0)",
        default = 0.25,
        smooth = "exp(5)"
    )]
    pub balance: FloatParam,

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

pub struct PingPong {
    params: Arc<PingPongParams>,
    sample_rate: f32,
    line_l: Vec<f32>,
    line_r: Vec<f32>,
    buffer_len: usize,
    write_pos: usize,
}

impl PingPong {
    pub fn new(params: Arc<PingPongParams>) -> Self {
        Self {
            params,
            sample_rate: 44_100.0,
            line_l: Vec::new(),
            line_r: Vec::new(),
            buffer_len: 1,
            write_pos: 0,
        }
    }
}

impl PluginLogic for PingPong {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        #[allow(clippy::cast_possible_truncation)]
        let sr = sample_rate as f32;
        self.sample_rate = sr;
        self.params.set_sample_rate(sample_rate);
        self.params.snap_smoothers();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let len = (MAX_DELAY_SECS * sr) as usize + 1;
        self.buffer_len = len.max(1);
        self.line_l = vec![0.0; self.buffer_len];
        self.line_r = vec![0.0; self.buffer_len];
        self.write_pos = 0;
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
        let buf_len = self.buffer_len;
        #[allow(clippy::cast_precision_loss)]
        let buf_len_f = buf_len as f32;
        let mut write = self.write_pos;
        let sr = self.sample_rate;
        let line_l = &mut self.line_l;
        let line_r = &mut self.line_r;
        let params = &self.params;

        buffer.for_each_frame::<2, _>(|frame_in: &[f32; 2], frame_out: &mut [f32; 2]| {
            let balance = params.balance.read();
            let delay_secs = params.delay_time.read();
            let feedback = params.feedback.read();
            let mix = params.mix.read();
            let delay_samples = delay_secs * sr;

            #[allow(clippy::cast_precision_loss)]
            let read_pos = (write as f32 - delay_samples + buf_len_f).rem_euclid(buf_len_f);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let r0 = read_pos.floor() as usize;

            let in_left = (1.0 - balance) * frame_in[0];
            let in_right = balance * frame_in[1];

            if r0 != write {
                let frac = read_pos - read_pos.floor();
                let dl0 = line_l[r0];
                let dl1 = line_l[(r0 + 1) % buf_len];
                let dr0 = line_r[r0];
                let dr1 = line_r[(r0 + 1) % buf_len];
                let del_l = dl0 + frac * (dl1 - dl0);
                let del_r = dr0 + frac * (dr1 - dr0);

                frame_out[0] = in_left + mix * (del_l - in_left);
                frame_out[1] = in_right + mix * (del_r - in_right);
                // The cross-feed (R into L's line) is the ping-pong.
                line_l[write] = in_left + del_r * feedback;
                line_r[write] = in_right + del_l * feedback;
            } else {
                frame_out[0] = in_left;
                frame_out[1] = in_right;
            }

            write += 1;
            if write >= buf_len {
                write -= buf_len;
            }
        });

        self.write_pos = write;
        ProcessStatus::Normal
    }

    fn editor(&self) -> Box<dyn Editor> {
        GridLayout::build(vec![widgets(vec![
            knob(P::Balance, "Bal"),
            knob(P::DelayTime, "Time"),
            knob(P::Feedback, "Fbk"),
            knob(P::Mix, "Mix"),
        ])])
        .with_title("PING-PONG")
        .into_editor(&self.params)
    }
}

truce::plugin! {
    logic: PingPong,
    params: PingPongParams,
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
}
