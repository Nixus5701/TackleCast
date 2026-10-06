//! Live measurement of TackleCast's own share of the capture-to-display
//! latency, for the FPS overlay.
//!
//! Only the part inside this PC is visible to software: from the moment a
//! frame's data arrives from the capture card to the moment it is presented,
//! plus an estimate of the wait for the next display refresh. Controller,
//! console, HDMI and capture-card encoding delay come before the first
//! timestamp and need a camera or photodiode to measure.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Timestamps a captured frame carries from the capture thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameTiming {
    /// The frame's data was read from the capture device.
    pub captured: Option<Instant>,
    /// Decoding finished and the frame was handed to the renderer.
    pub decoded: Option<Instant>,
}

/// One frame's path through TackleCast, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LatencySample {
    /// Device read to decoded (decode and, for MJPEG on the GPU, the copy).
    pub decode: f32,
    /// Decoded to picked up by the renderer (event loop, swapchain wait).
    pub queue: f32,
    /// Picked up to presented (upload, artifact reduction, VSR, drawing).
    pub render: f32,
    /// Estimated wait from present to the next display refresh.
    pub vblank: f32,
}

impl LatencySample {
    pub fn total(&self) -> f32 {
        self.decode + self.queue + self.render + self.vblank
    }
}

/// Rolling average over the last half second, refreshed a few times a second
/// so the overlay stays readable.
pub struct LatencyMeter {
    samples: VecDeque<(Instant, LatencySample)>,
    report: Option<LatencySample>,
    reported_at: Instant,
}

const WINDOW: Duration = Duration::from_millis(500);
const REPORT_INTERVAL: Duration = Duration::from_millis(250);

impl LatencyMeter {
    pub fn new() -> Self {
        Self { samples: VecDeque::new(), report: None, reported_at: Instant::now() }
    }

    pub fn record(&mut self, now: Instant, sample: LatencySample) {
        self.samples.push_back((now, sample));
        while self.samples.front().is_some_and(|(at, _)| now.duration_since(*at) > WINDOW) {
            self.samples.pop_front();
        }
        if self.report.is_none() || now.duration_since(self.reported_at) >= REPORT_INTERVAL {
            self.report = Some(average(self.samples.iter().map(|(_, s)| *s)));
            self.reported_at = now;
        }
    }

    /// The latest average, or `None` once frames have stopped arriving.
    pub fn report(&self) -> Option<LatencySample> {
        let fresh = self.samples.back().is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(1));
        if fresh { self.report } else { None }
    }
}

fn average(samples: impl Iterator<Item = LatencySample>) -> LatencySample {
    let (mut sum, mut count) = (LatencySample::default(), 0.0_f32);
    for s in samples {
        sum.decode += s.decode;
        sum.queue += s.queue;
        sum.render += s.render;
        sum.vblank += s.vblank;
        count += 1.0;
    }
    let count = count.max(1.0);
    LatencySample {
        decode: sum.decode / count,
        queue: sum.queue / count,
        render: sum.render / count,
        vblank: sum.vblank / count,
    }
}

pub fn millis(from: Instant, to: Instant) -> f32 {
    to.saturating_duration_since(from).as_secs_f32() * 1000.0
}

/// Milliseconds from now until the next display refresh, from the desktop
/// compositor's vblank clock. An estimate: it assumes the presented frame
/// makes that refresh, and in a window DWM may add one more.
#[cfg(windows)]
pub fn time_to_next_vblank() -> Option<f32> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    let (mut now, mut frequency) = (0_i64, 0_i64);
    unsafe {
        DwmGetCompositionTimingInfo(HWND::default(), &mut info).ok()?;
        QueryPerformanceCounter(&mut now).ok()?;
        QueryPerformanceFrequency(&mut frequency).ok()?;
    }
    vblank_wait(now as u64, info.qpcVBlank, info.qpcRefreshPeriod, frequency as u64)
}

#[cfg(not(windows))]
pub fn time_to_next_vblank() -> Option<f32> {
    None
}

/// Ticks until the next multiple of `period` after `last_vblank`, in ms.
fn vblank_wait(now: u64, last_vblank: u64, period: u64, frequency: u64) -> Option<f32> {
    if period == 0 || frequency == 0 {
        return None;
    }
    let since = now.wrapping_sub(last_vblank) % period;
    Some((period - since) as f32 * 1000.0 / frequency as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vblank_wait_wraps_into_the_refresh_period() {
        // 60 Hz with a 10 MHz counter: 166_667 ticks per refresh.
        let wait = vblank_wait(1_000_000 + 40_000, 1_000_000, 166_667, 10_000_000).unwrap();
        assert!((wait - 12.6667).abs() < 0.01, "{wait}");
        let later = vblank_wait(1_000_000 + 166_667 * 3 + 40_000, 1_000_000, 166_667, 10_000_000).unwrap();
        assert!((later - wait).abs() < 0.001);
        assert!(vblank_wait(1, 0, 0, 10).is_none());
    }

    #[test]
    fn meter_averages_recent_samples() {
        let mut meter = LatencyMeter::new();
        let start = Instant::now();
        let sample = |render| LatencySample { decode: 2.0, queue: 1.0, render, vblank: 4.0 };
        meter.record(start, sample(1.0));
        assert_eq!(meter.report().unwrap().total(), 8.0);
        meter.record(start + Duration::from_millis(300), sample(3.0));
        assert_eq!(meter.report().unwrap().render, 2.0);
        // Samples older than the window drop out of the next report.
        meter.record(start + Duration::from_millis(900), sample(5.0));
        assert_eq!(meter.report().unwrap().render, 5.0);
    }
}
