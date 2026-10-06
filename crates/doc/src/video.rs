//! Window › Timeline. A document-level playhead over a fixed number of frames at a frame rate.
//! Video layers (Layer › Video Layers) show their content for the current frame; this type is the
//! shared clock. Frame-animation and video-timeline modes both reduce to "which frame is current".

use serde::{Deserialize, Serialize};

use photocraft_raster::Surface;

/// Where a video layer's footage came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSource {
    /// Created blank (Layer › Video Layers › New Blank Video Layer).
    Blank,
    /// Loaded from a file or image sequence.
    File { path: String },
}

/// A video layer's frame stack (Layer › Video Layers). The layer's displayed content surface is a
/// cache of `frames[timeline.current]`, which the engine keeps in sync as the playhead moves.
///
/// Frames are full raster surfaces held in memory. (`.pcraft` persistence of the stack is a
/// follow-up; a saved document keeps the current frame as the layer's raster content.)
#[derive(Clone, Debug)]
pub struct VideoData {
    pub frames: Vec<Surface>,
    pub source: VideoSource,
    pub fps: f32,
    /// Show the altered (painted) frames rather than the original footage.
    pub show_altered: bool,
}

impl VideoData {
    pub fn new(frames: Vec<Surface>, fps: f32) -> Self {
        VideoData { frames: if frames.is_empty() { Vec::new() } else { frames }, source: VideoSource::Blank, fps, show_altered: true }
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

// Surfaces aren't `PartialEq` (and pixel comparison would be pointless here), so compare the stack
// by shape, not contents — enough for change detection.
impl PartialEq for VideoData {
    fn eq(&self, o: &Self) -> bool {
        self.frames.len() == o.frames.len() && self.source == o.source && self.fps == o.fps && self.show_altered == o.show_altered
    }
}

/// The document timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    /// Frames per second.
    pub fps: f32,
    /// Total number of frames (>= 1).
    pub duration: usize,
    /// The playhead (0-based frame index).
    pub current: usize,
    /// Work-area start frame (inclusive).
    #[serde(default)]
    pub work_start: usize,
    /// Work-area end frame (exclusive).
    pub work_end: usize,
}

impl Timeline {
    pub fn new(duration: usize, fps: f32) -> Self {
        let d = duration.max(1);
        Timeline { fps: if fps > 0.0 { fps } else { 30.0 }, duration: d, current: 0, work_start: 0, work_end: d }
    }

    /// Keep every index inside `0..duration` and the work area non-empty.
    pub fn clamp(&mut self) {
        self.duration = self.duration.max(1);
        self.fps = if self.fps > 0.0 { self.fps } else { 30.0 };
        self.current = self.current.min(self.duration - 1);
        self.work_start = self.work_start.min(self.duration - 1);
        self.work_end = self.work_end.clamp(self.work_start + 1, self.duration);
    }

    /// Playhead time in seconds.
    pub fn time(&self) -> f32 {
        self.current as f32 / self.fps.max(1e-3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_clamp() {
        let t = Timeline::new(0, 0.0);
        assert_eq!(t.duration, 1);
        assert_eq!(t.fps, 30.0);
        let mut t = Timeline::new(24, 24.0);
        t.current = 100;
        t.work_end = 999;
        t.clamp();
        assert_eq!(t.current, 23);
        assert_eq!(t.work_end, 24);
        assert!((t.time() - 23.0 / 24.0).abs() < 1e-6);
    }

    #[test]
    fn round_trips() {
        let t = Timeline::new(48, 25.0);
        let j = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Timeline>(&j).unwrap(), t);
    }
}
