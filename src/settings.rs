use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const FPS_MODE_30: &str = "30";
pub const FPS_MODE_60: &str = "60";
pub const FPS_MODE_120: &str = "120";
#[allow(dead_code)]
pub const FPS_MODE_CUSTOM: &str = "custom";
pub const MIN_FPS: u32 = 30;
pub const MAX_FPS: u32 = 240;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub presentation_mode: PresentationMode,
    #[serde(default)]
    pub image_adjustments: ImageAdjustments,
    #[serde(default)]
    pub video_device: String,
    #[serde(default = "default_scaling_filter")]
    pub scaling_filter: ScaleFilter,
    /// Optional NVIDIA driver request. Old settings files leave it off.
    #[serde(default)]
    pub request_super_resolution: bool,
    #[serde(default = "default_audio_index")]
    pub audio_input: i32,
    #[serde(default = "default_audio_index")]
    pub audio_output: i32,
    #[serde(default = "default_resolution")]
    pub resolution: String,
    #[serde(default = "default_fps_mode")]
    pub fps_mode: String,
    #[serde(default = "default_custom_fps")]
    pub custom_fps: u32,
    #[serde(default = "default_volume")]
    pub volume: f64,
    #[serde(default = "default_show_overlay")]
    pub show_overlay: bool,
    /// When set, the overlay also reports the active scaling filter, stacked
    /// over three lines. Off by default, keeping the overlay to one line.
    #[serde(default)]
    pub detailed_overlay: bool,
}

/// Display controls; sharpness is a post-VSR filter, not a driver parameter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageAdjustments {
    pub vsr_sharpness: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub hue: f32,
    pub gamma: f32,
    /// MJPEG artifact reduction strength in percent; 0 is off, 100 the tuned
    /// default. Only MJPEG frames are filtered.
    pub artifact_reduction: f32,
}
impl Default for ImageAdjustments {
    fn default() -> Self {
        Self { vsr_sharpness: 50.0, brightness: 0.0, contrast: 100.0,
               saturation: 100.0, hue: 0.0, gamma: 1.0, artifact_reduction: 100.0 }
    }
}
impl ImageAdjustments {
    pub fn sanitized(self) -> Self {
        fn limit(v: f32, low: f32, high: f32, fallback: f32) -> f32 {
            if v.is_finite() { v.clamp(low, high) } else { fallback }
        }
        Self {
            vsr_sharpness: limit(self.vsr_sharpness, 0.0, 100.0, 50.0),
            brightness: limit(self.brightness, -100.0, 100.0, 0.0),
            contrast: limit(self.contrast, 0.0, 200.0, 100.0),
            saturation: limit(self.saturation, 0.0, 200.0, 100.0),
            hue: limit(self.hue, -180.0, 180.0, 0.0),
            gamma: limit(self.gamma, 0.25, 3.0, 1.0),
            artifact_reduction: limit(self.artifact_reduction, 0.0, 200.0, 100.0),
        }
    }
    /// Eight scalars, matching the 32-byte ImageParams WGSL uniform.
    pub fn uniforms(self) -> [f32; 8] {
        let p = self.sanitized();
        let (sin, cos) = p.hue.to_radians().sin_cos();
        [p.brightness / 100.0, p.contrast / 100.0, p.saturation / 100.0,
         p.gamma, cos, sin, (p.vsr_sharpness - 50.0) / 50.0, 0.0]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pixel_format: &'static str,
    pub decode_threads: usize,
}

/// Upscaling filter applied to the video planes in the fragment shader.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScaleFilter {
    Bilinear,
    Bicubic,
    Lanczos,
}

impl ScaleFilter {
    /// The `filter_mode` value the shader branches on. Must stay in step with
    /// the `filter_mode` comparisons in `VIDEO_SHADER`.
    pub fn as_u32(self) -> u32 {
        match self {
            Self::Bilinear => 0,
            Self::Bicubic => 1,
            Self::Lanczos => 2,
        }
    }

    /// Every variant, in menu order.
    pub const ALL: [Self; 3] = [Self::Bilinear, Self::Bicubic, Self::Lanczos];
}

impl Display for ScaleFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bilinear => f.write_str("Bilinear"),
            Self::Bicubic => f.write_str("Bicubic"),
            Self::Lanczos => f.write_str("Lanczos"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PresentationMode {
    #[default]
    Mailbox,
    #[serde(rename = "mailbox_low_latency")]
    MailboxLowLatency,
    Immediate,
}
impl PresentationMode {
    pub const ALL: [Self; 3] = [Self::Mailbox, Self::MailboxLowLatency, Self::Immediate];
    pub fn label(self) -> &'static str {
        match self {
            Self::Mailbox => "Mailbox (default)",
            Self::MailboxLowLatency => "Mailbox (low latency)",
            Self::Immediate => "Immediate (lowest latency; may tear)",
        }
    }
    /// Whether the renderer waits for the previous frame's GPU work before
    /// starting the next, so frames can't queue on the GPU. Neither low-latency
    /// mode wants a GPU backlog; plain Mailbox keeps the original pipelining.
    pub fn gates_gpu_queue(self) -> bool {
        matches!(self, Self::MailboxLowLatency | Self::Immediate)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            presentation_mode: PresentationMode::default(),
            image_adjustments: ImageAdjustments::default(),
            video_device: String::new(),
            scaling_filter: default_scaling_filter(),
            request_super_resolution: false,
            audio_input: default_audio_index(),
            audio_output: default_audio_index(),
            resolution: default_resolution(),
            fps_mode: default_fps_mode(),
            custom_fps: default_custom_fps(),
            volume: default_volume(),
            show_overlay: default_show_overlay(),
            detailed_overlay: false,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };

        let mut settings: Self = serde_json::from_str(&raw).unwrap_or_default();
        settings.image_adjustments = settings.image_adjustments.sanitized();
        settings
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)
            .expect("settings serialization should not fail");
        fs::write(path, json)
    }

    pub fn get_fps(&self) -> u32 {
        match self.fps_mode.as_str() {
            FPS_MODE_30 => 30,
            FPS_MODE_60 => 60,
            FPS_MODE_120 => 120,
            _ => self.custom_fps.clamp(MIN_FPS, MAX_FPS),
        }
    }

    /// Update resolution and fps_mode to reflect what the capture device
    /// actually negotiated (e.g. after fallback to a lower resolution/fps).
    pub fn apply_negotiated(&mut self, width: u32, height: u32, fps: u32) {
        let new_resolution = match (width, height) {
            (3840, 2160) => "4K",
            (2560, 1440) => "1440p",
            (1280, 720) => "720p",
            _ => "1080p",
        };
        let new_fps_mode = match fps {
            30 => FPS_MODE_30,
            120 => FPS_MODE_120,
            60 => FPS_MODE_60,
            other => {
                self.custom_fps = other;
                FPS_MODE_CUSTOM
            }
        };

        self.resolution = new_resolution.to_string();
        self.fps_mode = new_fps_mode.to_string();
    }
}

pub fn get_capture_config(resolution: &str, fps: u32) -> CaptureConfig {
    let (width, height) = match resolution {
        "720p" => (1280, 720),
        "1440p" => (2560, 1440),
        "4K" => (3840, 2160),
        _ => (1920, 1080),
    };

    if fps <= 60 {
        CaptureConfig {
            width,
            height,
            fps,
            pixel_format: "nv12",
            decode_threads: 1,
        }
    } else {
        CaptureConfig {
            width,
            height,
            fps,
            pixel_format: "mjpeg",
            decode_threads: 4,
        }
    }
}

pub fn settings_path() -> PathBuf {
    let mut base = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    if cfg!(debug_assertions) {
        if let Ok(current_dir) = std::env::current_dir() {
            base = current_dir;
        }
    }

    base.join("tacklecast_settings.json")
}

fn default_audio_index() -> i32 {
    -1
}

fn default_scaling_filter() -> ScaleFilter {
    ScaleFilter::Bilinear
}

fn default_resolution() -> String {
    "1080p".to_string()
}

fn default_fps_mode() -> String {
    FPS_MODE_60.to_string()
}

fn default_custom_fps() -> u32 {
    120
}

fn default_volume() -> f64 {
    1.0
}

fn default_show_overlay() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_controls_migrate_and_round_trip() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.image_adjustments.uniforms(), [0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
        let partial: Settings = serde_json::from_str(r#"{"image_adjustments":{"vsr_sharpness":20.0}}"#).unwrap();
        assert_eq!(partial.image_adjustments.gamma, 1.0);
        assert_eq!(partial.image_adjustments.contrast, 100.0);
        assert_eq!(partial.image_adjustments.artifact_reduction, 100.0);
        let p = ImageAdjustments { vsr_sharpness: 20.0, brightness: -12.0, contrast: 115.0,
            saturation: 90.0, hue: 25.0, gamma: 1.2, artifact_reduction: 60.0 };
        let settings = Settings { image_adjustments: p, ..Settings::default() };
        let decoded: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(decoded.image_adjustments, p);
    }

    #[test]
    fn invalid_image_controls_cannot_send_nonfinite_values_to_gpu() {
        let bad = ImageAdjustments { vsr_sharpness: -400.0, brightness: f32::NAN,
            contrast: f32::INFINITY, saturation: -5.0, hue: 500.0, gamma: 0.0,
            artifact_reduction: f32::NAN };
        let p = bad.sanitized();
        assert_eq!(p, ImageAdjustments { vsr_sharpness: 0.0, brightness: 0.0,
            contrast: 100.0, saturation: 0.0, hue: 180.0, gamma: 0.25, artifact_reduction: 100.0 });
        assert!(bad.uniforms().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn default_settings_round_trip() {
        let settings = Settings::default();
        let json = serde_json::to_string_pretty(&settings).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, settings);
    }

    #[test]
    fn python_settings_shape_deserializes() {
        let json = r#"{
  "video_device": "ShadowCast 3",
  "scaling_filter": "bicubic",
  "audio_input": 15,
  "audio_output": 12,
  "resolution": "1440p",
  "fps_mode": "120",
  "custom_fps": 120,
  "volume": 1.0,
  "show_overlay": true
}"#;

        let settings: Settings = serde_json::from_str(json).unwrap();
        assert!(!settings.request_super_resolution);
        assert_eq!(settings.video_device, "ShadowCast 3");
        assert_eq!(settings.scaling_filter, ScaleFilter::Bicubic);
        assert_eq!(settings.audio_input, 15);
        assert_eq!(settings.audio_output, 12);
        assert_eq!(settings.resolution, "1440p");
        assert_eq!(settings.fps_mode, "120");
        assert_eq!(settings.get_fps(), 120);
        assert!(settings.show_overlay);
    }

    #[test]
    fn super_resolution_setting_persists() {
        let settings = Settings { request_super_resolution: true, ..Settings::default() };
        let decoded: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(decoded.request_super_resolution);
    }

    #[test]
    fn capture_config_matches_python_logic() {
        let nv12 = get_capture_config("1080p", 60);
        assert_eq!(nv12.pixel_format, "nv12");
        assert_eq!(nv12.decode_threads, 1);

        let mjpeg = get_capture_config("1440p", 120);
        assert_eq!(mjpeg.width, 2560);
        assert_eq!(mjpeg.height, 1440);
        assert_eq!(mjpeg.pixel_format, "mjpeg");
        assert_eq!(mjpeg.decode_threads, 4);
    }
}

