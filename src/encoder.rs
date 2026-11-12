use crate::Result;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};
use windows_capture::{
    encoder::{
        AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
        VideoSettingsSubType,
    },
    frame::Frame,
};

pub fn create(width: u32, height: u32, path: &Path) -> Result<VideoEncoder> {
    Ok(VideoEncoder::new(
        VideoSettingsBuilder::new(width + width % 2, height + height % 2)
            .sub_type(VideoSettingsSubType::H264)
            .frame_rate(30)
            .bitrate(6_000_000),
        AudioSettingsBuilder::new().bitrate(160_000),
        ContainerSettingsBuilder::new(),
        path,
    )?)
}

pub struct Segment {
    pub encoder: VideoEncoder,
    pub path: PathBuf,
    pub start: i64,
    pub end: Option<i64>,
}
pub struct Pending {
    pub encoder: VideoEncoder,
    pub path: PathBuf,
    pub at: i64,
}

// Both capture threads share this router. Segment boundaries are video-frame
// timestamps; delayed audio is split at the same boundary before finalization.
pub struct Router {
    segments: VecDeque<Segment>,
    pub pending: Option<Pending>,
    pub epoch: i64,
    audio_cursor: i64,
}
impl Router {
    pub fn new(encoder: VideoEncoder, path: PathBuf) -> Self {
        Self {
            segments: VecDeque::from([Segment {
                encoder,
                path,
                start: 0,
                end: None,
            }]),
            pending: None,
            epoch: 0,
            audio_cursor: 0,
        }
    }
    pub fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let ts = frame.timestamp()?.Duration;
        if self.epoch == 0 {
            self.epoch = ts;
            self.segments.back_mut().ok_or("Нет кодировщика")?.start = ts;
        }
        if self.pending.as_ref().is_some_and(|p| ts >= p.at) {
            let next = self.pending.take().unwrap();
            self.segments.back_mut().unwrap().end = Some(ts);
            self.segments.push_back(Segment {
                encoder: next.encoder,
                path: next.path,
                start: ts,
                end: None,
            });
        }
        self.segments
            .back_mut()
            .ok_or("Нет кодировщика")?
            .encoder
            .send_frame(frame)?;
        Ok(())
    }
    pub fn send_audio(&mut self, bytes: &[u8], start_sample: i64) -> Result<()> {
        let frames = bytes.len() as i64 / 4;
        for segment in &mut self.segments {
            let start = sample_at(segment.start, self.epoch);
            let end = segment
                .end
                .map(|end| sample_at(end, self.epoch))
                .unwrap_or(i64::MAX);
            let from = start_sample.max(start);
            let until = (start_sample + frames).min(end);
            if until > from {
                let first = ((from - start_sample) * 4) as usize;
                let last = ((until - start_sample) * 4) as usize;
                segment.encoder.send_audio_buffer(&bytes[first..last], 0)?;
            }
        }
        self.audio_cursor = start_sample + frames;
        Ok(())
    }
    pub fn active_start(&self) -> i64 {
        self.segments.back().map(|s| s.start).unwrap_or(0)
    }
    pub fn take_completed(&mut self) -> Option<Segment> {
        if self
            .segments
            .front()
            .and_then(|s| s.end)
            .is_some_and(|end| self.audio_cursor >= sample_at(end, self.epoch))
        {
            self.segments.pop_front()
        } else {
            None
        }
    }
    pub fn finish_all(&mut self) -> Result<()> {
        // An unused prepared encoder has no frames; its Drop closes the channels.
        self.pending.take();
        let mut error = None;
        for segment in self.segments.drain(..) {
            if let Err(e) = segment.encoder.finish() {
                error = Some(e);
            }
        }
        error.map_or(Ok(()), |e| Err(e.into()))
    }
}
fn sample_at(timestamp: i64, epoch: i64) -> i64 {
    (((timestamp - epoch) as i128 * 48_000) / 10_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_boundaries_use_the_common_clock() {
        assert_eq!(sample_at(110_000_000, 100_000_000), 48_000);
        assert_eq!(sample_at(100_100_000, 100_000_000), 480);
    }
}
