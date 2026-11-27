use komponist_processor::{
    Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};

/// An effect that writes only its delayed signal. Its buffer is allocated off the audio thread.
pub struct FeedbackDelay {
    line: Vec<[f32; 2]>,
    feedback: f32,
    position: usize,
}

impl FeedbackDelay {
    pub fn new(frames: usize, feedback: f32) -> Self {
        Self {
            line: vec![[0.0; 2]; frames],
            feedback,
            position: 0,
        }
    }
}

impl Processor for FeedbackDelay {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(&mut self, _: &ProcessContext, _: &[Event], io: &mut StereoIo<'_>) -> ProcessStatus {
        for i in 0..io.frames() {
            let frame = &mut self.line[self.position];
            for (channel, sample) in frame.iter_mut().enumerate() {
                io.output[channel][i] = *sample;
                *sample = io.input[channel][i] + self.feedback * *sample;
            }
            self.position += 1;
            if self.position == self.line.len() {
                self.position = 0;
            }
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {
        self.line.fill([0.0; 2]);
        self.position = 0;
    }
}
