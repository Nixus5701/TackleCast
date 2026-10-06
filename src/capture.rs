use std::f32::consts::PI;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use ffmpeg_next as ffmpeg;
use ffmpeg::{
    codec, device, format, media, threading, Dictionary, Packet,
    software::scaling::{flag::Flags as ScaleFlags, Context as ScaleContext},
    util::frame::video::Video,
};
use tracing::{info, warn};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};
use winit::event_loop::EventLoopProxy;

use crate::jpeg_quant::JpegQuant;
use crate::AppEvent;
use crate::triple_buffer::Producer;

/// How long `CaptureThread::stop` waits for the capture thread to notice the
/// stop flag. DirectShow reads block until the device delivers a frame, so a
/// source that has gone quiet (console off, HDMI unplugged at the source) can
/// hold the thread indefinitely. Past this point the thread is detached rather
/// than freezing the UI; it exits on its own at the next frame, or with the
/// process.
const STOP_TIMEOUT: Duration = Duration::from_millis(2500);

/// Consecutive failed packet reads (other than "try again") after which the
/// device is treated as lost. Unplugging a DirectShow device makes every read
/// fail immediately with EIO.
const MAX_CONSECUTIVE_READ_ERRORS: u32 = 100;

/// Why an opened-or-attempted capture session ended.
enum CaptureError {
    /// The device could not be opened or decoded with these settings. The
    /// caller moves on to the next format/resolution fallback.
    Open(String),
    /// The device opened and streamed, then stopped working (unplugged,
    /// driver error). Falling back to other settings would only downgrade the
    /// user's saved configuration, so this is reported instead.
    Lost(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Nv12,
    Yuvj422p,
}

#[derive(Debug, Clone)]
pub enum CaptureFrame {
    /// Frame data lives in CPU memory (software decode or GPU decode fallback).
    Cpu {
        width: u32,
        height: u32,
        format: PixelFormat,
        y_data: Vec<u8>,
        u_data: Vec<u8>,
        v_data: Vec<u8>,
        /// Quantization tables when the frame was decoded from MJPEG.
        quant: Option<JpegQuant>,
    },
    /// Frame data lives in a shared DX12/CUDA GPU buffer (zero-copy path).
    /// The renderer knows where the actual buffers are; this just carries
    /// the dimensions and which double-buffer set to read from.
    #[cfg(feature = "gpu-decode")]
    Gpu {
        width: u32,
        height: u32,
        buffer_index: usize,
        // Retained through GPU copy completion before CUDA may reuse this set.
        lease: Arc<()>,
        quant: Option<JpegQuant>,
    },
}

impl CaptureFrame {
    /// Create an empty frame for triple buffer slot initialization.
    pub fn empty() -> Self {
        Self::Cpu {
            width: 0,
            height: 0,
            format: PixelFormat::Nv12,
            y_data: Vec::new(),
            u_data: Vec::new(),
            v_data: Vec::new(),
            quant: None,
        }
    }

    /// Reshape this frame into a `Cpu` frame with the given geometry, keeping
    /// the existing plane buffers when it already is one, and return the planes
    /// for the caller to fill.
    ///
    /// `Vec::resize` is a no-op once capacity is sufficient, so a frame
    /// recycled from the triple buffer is reshaped without allocating.
    pub fn reshape_cpu(
        &mut self,
        new_width: u32,
        new_height: u32,
        new_format: PixelFormat,
        y_len: usize,
        u_len: usize,
        v_len: usize,
    ) -> (&mut [u8], &mut [u8], &mut [u8]) {
        if !matches!(self, Self::Cpu { .. }) {
            *self = Self::empty();
        }

        match self {
            Self::Cpu {
                width,
                height,
                format,
                y_data,
                u_data,
                v_data,
                quant,
            } => {
                *quant = None;
                *width = new_width;
                *height = new_height;
                *format = new_format;
                y_data.resize(y_len, 0);
                u_data.resize(u_len, 0);
                v_data.resize(v_len, 0);
                (
                    y_data.as_mut_slice(),
                    u_data.as_mut_slice(),
                    v_data.as_mut_slice(),
                )
            }
            #[cfg(feature = "gpu-decode")]
            Self::Gpu { .. } => unreachable!("coerced to Cpu above"),
        }
    }

    /// The MJPEG quantization tables this frame was decoded with, if any.
    pub fn quant(&self) -> Option<&JpegQuant> {
        match self {
            Self::Cpu { quant, .. } => quant.as_ref(),
            #[cfg(feature = "gpu-decode")]
            Self::Gpu { quant, .. } => quant.as_ref(),
        }
    }

    pub fn set_quant(&mut self, value: Option<JpegQuant>) {
        match self {
            Self::Cpu { quant, .. } => *quant = value,
            #[cfg(feature = "gpu-decode")]
            Self::Gpu { quant, .. } => *quant = value,
        }
    }

    pub fn width(&self) -> u32 {
        match self {
            Self::Cpu { width, .. } => *width,
            #[cfg(feature = "gpu-decode")]
            Self::Gpu { width, .. } => *width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Self::Cpu { height, .. } => *height,
            #[cfg(feature = "gpu-decode")]
            Self::Gpu { height, .. } => *height,
        }
    }

    pub fn format(&self) -> Option<PixelFormat> {
        match self {
            Self::Cpu { format, .. } => Some(*format),
            #[cfg(feature = "gpu-decode")]
            Self::Gpu { .. } => Some(PixelFormat::Yuvj422p), // nvJPEG always outputs YUV 4:2:2
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureStats {
    pub fps: f32,
    pub width: u32,
    pub height: u32,
}

/// Sent once when the capture thread successfully opens a device at settings
/// different from what was originally requested (resolution/fps fallback).
#[derive(Debug, Clone, Copy)]
pub struct NegotiatedConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub source: CaptureSource,
}

pub enum CaptureSource {
    TestPattern {
        alternate_formats: bool,
        force_format: Option<PixelFormat>,
    },
    DirectShow {
        device_name: String,
        pixel_format: String,
        decode_threads: usize,
        /// Shared DX12 buffer handles for zero-copy GPU decode.
        /// When present, the capture thread will try to use these for
        /// nvJPEG decode directly into GPU memory. The handles are consumed
        /// (imported + closed) during decoder initialization.
        #[cfg(feature = "gpu-decode")]
        shared_gpu_handles: Option<crate::dx12_interop::ImportHandles>,
    },
}

pub struct CaptureThread {
    frame_consumer: crate::triple_buffer::Consumer<CaptureFrame>,
    stats_rx: Receiver<CaptureStats>,
    error_rx: Receiver<String>,
    negotiated_rx: Receiver<NegotiatedConfig>,
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl CaptureThread {
    pub fn start(config: CaptureConfig, event_proxy: EventLoopProxy<AppEvent>) -> Self {
        let (frame_producer, frame_consumer) =
            crate::triple_buffer::triple_buffer(CaptureFrame::empty);
        let (stats_tx, stats_rx) = unbounded();
        let (error_tx, error_rx) = unbounded();
        let (negotiated_tx, negotiated_rx) = unbounded();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let thread_stop_flag = stop_flag.clone();

        let join_handle = thread::spawn(move || match config.source {
            CaptureSource::TestPattern {
                alternate_formats,
                force_format,
            } => run_test_pattern(
                config.width,
                config.height,
                config.fps.max(1),
                alternate_formats,
                force_format,
                thread_stop_flag,
                frame_producer,
                stats_tx,
                &event_proxy,
            ),
            CaptureSource::DirectShow {
                device_name,
                pixel_format,
                decode_threads,
                #[cfg(feature = "gpu-decode")]
                shared_gpu_handles,
            } => run_directshow_capture(
                device_name,
                config.width,
                config.height,
                config.fps.max(1),
                pixel_format,
                decode_threads,
                thread_stop_flag,
                frame_producer,
                stats_tx,
                error_tx,
                negotiated_tx,
                &event_proxy,
                #[cfg(feature = "gpu-decode")]
                shared_gpu_handles,
            ),
        });

        Self {
            frame_consumer,
            stats_rx,
            error_rx,
            negotiated_rx,
            stop_flag,
            join_handle: Some(join_handle),
        }
    }

    /// Borrows the latest frame if the capture thread has produced one since
    /// the last call. Returns None if already up-to-date.
    ///
    /// The frame stays in its triple-buffer slot so its plane buffers can be
    /// reused for a later frame — hence the borrow rather than an owned value.
    pub fn latest_frame(&mut self) -> Option<&CaptureFrame> {
        self.frame_consumer.read()
    }

    pub fn latest_stats(&self) -> Option<CaptureStats> {
        self.stats_rx.try_iter().last()
    }

    pub fn latest_error(&self) -> Option<String> {
        self.error_rx.try_iter().last()
    }

    pub fn latest_negotiated(&self) -> Option<NegotiatedConfig> {
        self.negotiated_rx.try_iter().last()
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        let Some(join_handle) = self.join_handle.take() else {
            return;
        };
        let deadline = Instant::now() + STOP_TIMEOUT;
        while !join_handle.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if join_handle.is_finished() {
            let _ = join_handle.join();
        } else {
            warn!(
                "capture thread did not stop within {:?} (device is delivering no frames); detaching it",
                STOP_TIMEOUT
            );
        }
    }
}

impl Drop for CaptureThread {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_test_pattern(
    width: u32,
    height: u32,
    fps: u32,
    alternate_formats: bool,
    force_format: Option<PixelFormat>,
    stop_flag: Arc<AtomicBool>,
    mut frame_producer: Producer<CaptureFrame>,
    stats_tx: Sender<CaptureStats>,
    event_proxy: &EventLoopProxy<AppEvent>,
) {
    let frame_interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut frame_index = 0_u64;
    let mut last_stats_at = Instant::now();
    let mut stats_frame_counter = 0_u32;

    while !stop_flag.load(Ordering::Relaxed) {
        let loop_started = Instant::now();
        let format = force_format.unwrap_or_else(|| {
            if alternate_formats && ((frame_index / fps as u64) % 6) >= 3 {
                PixelFormat::Yuvj422p
            } else {
                PixelFormat::Nv12
            }
        });

        let frame = generate_test_frame(width, height, frame_index, format);
        frame_producer.write(frame);
        let _ = event_proxy.send_event(AppEvent::FrameReady);

        frame_index += 1;
        stats_frame_counter += 1;

        let elapsed = last_stats_at.elapsed();
        if elapsed >= Duration::from_millis(300) {
            let fps = stats_frame_counter as f32 / elapsed.as_secs_f32();
            let _ = stats_tx.send(CaptureStats { fps, width, height });
            last_stats_at = Instant::now();
            stats_frame_counter = 0;
        }

        let spent = loop_started.elapsed();
        if spent < frame_interval {
            thread::sleep(frame_interval - spent);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_directshow_capture(
    device_name: String,
    requested_width: u32,
    requested_height: u32,
    requested_fps: u32,
    requested_pixel_format: String,
    decode_threads: usize,
    stop_flag: Arc<AtomicBool>,
    mut frame_producer: Producer<CaptureFrame>,
    stats_tx: Sender<CaptureStats>,
    error_tx: Sender<String>,
    negotiated_tx: Sender<NegotiatedConfig>,
    event_proxy: &EventLoopProxy<AppEvent>,
    #[cfg(feature = "gpu-decode")]
    mut shared_gpu_handles: Option<crate::dx12_interop::ImportHandles>,
) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
    let _mmcss = crate::mmcss::MmcssRegistration::register("Capture");
    ffmpeg::log::set_level(ffmpeg::log::Level::Error);

    // Build list of (width, height, fps) tiers to try. Start with the
    // requested settings, then fall back to common resolutions/framerates
    // that most USB capture devices and webcams support.
    let resolution_tiers = resolution_fallback_tiers(
        requested_width,
        requested_height,
        requested_fps,
    );

    let mut last_error: Option<String> = None;

    for &(try_w, try_h, try_fps) in &resolution_tiers {
        let attempt_formats = pixel_format_attempts(&requested_pixel_format);

        for format_attempt in attempt_formats {
            if stop_flag.load(Ordering::Relaxed) {
                return;
            }

            let format_label = format_attempt.as_deref().unwrap_or("auto");
            info!(
                "capture attempt: device='{}' format='{}' {}x{} @ {}fps",
                device_name, format_label, try_w, try_h, try_fps
            );

            match run_directshow_capture_inner(
                &device_name,
                try_w,
                try_h,
                try_fps,
                format_attempt.as_deref(),
                decode_threads,
                stop_flag.clone(),
                &mut frame_producer,
                stats_tx.clone(),
                event_proxy,
                #[cfg(feature = "gpu-decode")]
                &mut shared_gpu_handles,
            ) {
                Ok(()) => {
                    // Notify if we fell back to different settings
                    if try_w != requested_width || try_h != requested_height || try_fps != requested_fps {
                        info!(
                            "capture negotiated fallback: {}x{} @ {}fps (requested {}x{} @ {}fps)",
                            try_w, try_h, try_fps, requested_width, requested_height, requested_fps
                        );
                        let _ = negotiated_tx.send(NegotiatedConfig {
                            width: try_w,
                            height: try_h,
                            fps: try_fps,
                        });
                    }
                    return;
                }
                Err(CaptureError::Lost(error)) => {
                    warn!("capture device lost: device='{}' error={}", device_name, error);
                    let _ = error_tx.send(error);
                    return;
                }
                Err(CaptureError::Open(error)) => {
                    warn!(
                        "capture attempt failed: device='{}' format='{}' {}x{} @ {}fps error={}",
                        device_name, format_label, try_w, try_h, try_fps, error
                    );
                    last_error = Some(error);
                }
            }
        }
    }

    let _ = error_tx.send(
        last_error.unwrap_or_else(|| format!("all capture attempts failed for '{device_name}'")),
    );
}

#[allow(clippy::too_many_arguments)]
fn run_directshow_capture_inner(
    device_name: &str,
    requested_width: u32,
    requested_height: u32,
    requested_fps: u32,
    requested_pixel_format: Option<&str>,
    decode_threads: usize,
    stop_flag: Arc<AtomicBool>,
    frame_producer: &mut Producer<CaptureFrame>,
    stats_tx: Sender<CaptureStats>,
    event_proxy: &EventLoopProxy<AppEvent>,
    #[cfg(feature = "gpu-decode")]
    shared_gpu_handles: &mut Option<crate::dx12_interop::ImportHandles>,
) -> Result<(), CaptureError> {
    let dshow_format = find_dshow_format().ok_or_else(|| {
        CaptureError::Open("DirectShow input format was not found in FFmpeg".to_string())
    })?;
    let mut options = Dictionary::new();
    options.set("video_size", &format!("{requested_width}x{requested_height}"));
    options.set("framerate", &requested_fps.to_string());
    options.set("rtbufsize", "16M");
    options.set("probesize", "5000000");
    options.set("analyzeduration", "1000000");
    if let Some(format_name) = requested_pixel_format {
        if format_name.eq_ignore_ascii_case("mjpeg") {
            options.set("vcodec", "mjpeg");
        } else {
            options.set("pixel_format", format_name);
        }
    }

    let url = format!("video={device_name}");
    let mut input = format::open_with(&url, &dshow_format, options)
        .map_err(|error| CaptureError::Open(format!("failed to open DirectShow input for '{device_name}': {error}")))?
        .input();

    let input_stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| CaptureError::Open(format!("no video stream found for '{device_name}'")))?;
    let stream_index = input_stream.index();
    let parameters = input_stream.parameters();

    let mut decoder_context = codec::context::Context::from_parameters(parameters)
        .map_err(|error| CaptureError::Open(format!("failed to create decoder context: {error}")))?;
    // Slice threading, never frame threading: frame threading holds back
    // (threads - 1) frames inside the decoder, which is pure added latency.
    // FFmpeg's MJPEG and rawvideo decoders are single-threaded either way.
    decoder_context.set_threading(threading::Config {
        kind: threading::Type::Slice,
        count: decode_threads.max(1),
    });
    decoder_context.set_flags(codec::Flags::LOW_DELAY);

    let mut decoder = decoder_context
        .decoder()
        .video()
        .map_err(|error| CaptureError::Open(format!("failed to open video decoder: {error}")))?;

    // Log actual stream parameters vs requested
    let actual_rate = input_stream.rate();
    let actual_fps_f64 = if actual_rate.1 > 0 {
        actual_rate.0 as f64 / actual_rate.1 as f64
    } else {
        0.0
    };
    let actual_width = decoder.width();
    let actual_height = decoder.height();
    info!(
        "stream negotiated: {}x{} @ {:.2}fps (time_base={}/{}), requested: {}x{} @ {}fps",
        actual_width, actual_height, actual_fps_f64,
        actual_rate.0, actual_rate.1,
        requested_width, requested_height, requested_fps,
    );
    if (actual_fps_f64 - requested_fps as f64).abs() > 1.0 && actual_fps_f64 > 0.0 {
        warn!(
            "stream framerate mismatch: requested {}fps but device negotiated {:.1}fps",
            requested_fps, actual_fps_f64
        );
    }

    let mut decoded = Video::empty();
    let mut last_stats_at = Instant::now();
    let mut last_summary_at = Instant::now();
    let mut summary_frame_counter = 0_u64;
    let mut stats_frame_counter = 0_u32;
    let mut total_frames = 0_u64;
    let mut logged_first_frame = false;
    let mut packet_errors = 0_u32;

    // Frame arrival timing
    let mut last_packet_at: Option<Instant> = None;
    let mut arrival_min_us = u64::MAX;
    let mut arrival_max_us = 0_u64;
    let mut arrival_sum_us = 0_u64;
    let mut arrival_count = 0_u64;
    let mut scaler: Option<ScaleContext> = None;
    let mut scaled_frame = Video::empty();

    // Try to initialize GPU-accelerated MJPEG decode (NVIDIA nvJPEG)
    #[cfg(feature = "gpu-decode")]
    let mut gpu_decoder = if requested_pixel_format.is_some_and(|f| f.eq_ignore_ascii_case("mjpeg")) {
        // Try zero-copy shared buffer mode first, then fall back to owned mode
        let shared_compatible = shared_gpu_handles.as_ref().is_some_and(|handles| {
            handles.layout.width == actual_width && handles.layout.height == actual_height
        });
        if shared_compatible {
            let handles = shared_gpu_handles.take().expect("compatible shared handles");
            info!("attempting zero-copy GPU decode with shared DX12 buffers");
            crate::gpu_decode::NvjpegDecoder::try_new_shared(handles)
                .or_else(|| {
                    info!("zero-copy init failed, falling back to owned GPU decode");
                    crate::gpu_decode::NvjpegDecoder::try_new()
                })
        } else {
            crate::gpu_decode::NvjpegDecoder::try_new()
        }
    } else {
        None
    };
    #[cfg(feature = "gpu-decode")]
    let use_gpu = gpu_decoder.is_some();
    #[cfg(feature = "gpu-decode")]
    let is_zero_copy = gpu_decoder.as_ref().map(|d| d.is_zero_copy()).unwrap_or(false);
    #[cfg(feature = "gpu-decode")]
    let mut gpu_errors = 0_u32;
    #[cfg(not(feature = "gpu-decode"))]
    let use_gpu = false;
    #[cfg(not(feature = "gpu-decode"))]
    let is_zero_copy = false;

    info!(
        "capture thread opened DirectShow stream for '{}' at {}x{} @ {}fps using {} (gpu_decode={}, zero_copy={})",
        device_name,
        requested_width,
        requested_height,
        requested_fps,
        requested_pixel_format.unwrap_or("auto"),
        use_gpu,
        is_zero_copy,
    );

    // MJPEG and raw frames are each self-contained, so when several packets
    // are already queued only the newest is worth decoding. Codecs with
    // inter-frame prediction (H.264 from webcams) must see every packet.
    let is_mjpeg = decoder.id() == codec::Id::MJPEG;
    let drain_backlog = is_mjpeg || decoder.id() == codec::Id::RAWVIDEO;
    let mut stale_packets_skipped = 0_u64;
    let mut consecutive_read_errors = 0_u32;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Read packets directly rather than through `input.packets()`: that
        // iterator retries every error except EOF forever, so an unplugged
        // device (EIO on every read) spun a core at 100% and the stop flag
        // was never checked again, hanging `CaptureThread::stop`.
        let mut packet = Packet::empty();
        match packet.read(&mut input) {
            Ok(()) => consecutive_read_errors = 0,
            Err(ffmpeg::Error::Eof) => {
                return Err(CaptureError::Lost(format!(
                    "capture stream from '{device_name}' ended"
                )));
            }
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(error) => {
                consecutive_read_errors += 1;
                if consecutive_read_errors >= MAX_CONSECUTIVE_READ_ERRORS {
                    return Err(CaptureError::Lost(format!(
                        "capture device '{device_name}' stopped responding: {error}"
                    )));
                }
                thread::sleep(Duration::from_millis(1));
                continue;
            }
        }

        if packet.stream() != stream_index {
            continue;
        }

        if drain_backlog {
            stale_packets_skipped += take_newest_queued_packet(&mut input, stream_index, &mut packet);
        }

        // Track frame arrival timing
        let now = Instant::now();
        if let Some(prev) = last_packet_at {
            let interval_us = now.duration_since(prev).as_micros() as u64;
            arrival_min_us = arrival_min_us.min(interval_us);
            arrival_max_us = arrival_max_us.max(interval_us);
            arrival_sum_us += interval_us;
            arrival_count += 1;
        }
        last_packet_at = Some(now);

        // Try GPU decode first when available (MJPEG only)
        #[cfg(feature = "gpu-decode")]
        if let Some(ref mut gpu) = gpu_decoder {
            if let Some(data) = packet.data() {
                // Decode straight into the back slot, reusing the plane buffers
                // the previous frame in that slot left behind. Bound to a local
                // so the borrow ends before the arms touch the producer again.
                let decoded = gpu.decode_into(data, frame_producer.back_slot());

                match decoded {
                    Ok(()) => {
                        let slot = frame_producer.back_slot();
                        let width = slot.width();
                        let height = slot.height();
                        let format = slot.format();
                        total_frames += 1;
                        gpu_errors = 0; // reset consecutive error count on success

                        if !logged_first_frame {
                            logged_first_frame = true;
                            info!(
                                "first GPU-decoded frame from '{}' => {}x{} {:?}",
                                device_name, width, height, format
                            );
                        }

                        frame_producer.publish();
                        let _ = event_proxy.send_event(AppEvent::FrameReady);

                        stats_frame_counter += 1;
                        summary_frame_counter += 1;
                        let elapsed = last_stats_at.elapsed();
                        if elapsed >= Duration::from_millis(300) {
                            let fps = stats_frame_counter as f32 / elapsed.as_secs_f32();
                            let _ = stats_tx.send(CaptureStats { fps, width, height });
                            last_stats_at = Instant::now();
                            stats_frame_counter = 0;
                        }
                        let summary_elapsed = last_summary_at.elapsed();
                        if summary_elapsed >= Duration::from_secs(30) {
                            let avg_fps = summary_frame_counter as f64 / summary_elapsed.as_secs_f64();
                            let arrival_info = if arrival_count > 0 {
                                let avg_ms = (arrival_sum_us as f64 / arrival_count as f64) / 1000.0;
                                let min_ms = arrival_min_us as f64 / 1000.0;
                                let max_ms = arrival_max_us as f64 / 1000.0;
                                format!(", frame_arrival: avg={:.1}ms min={:.1}ms max={:.1}ms", avg_ms, min_ms, max_ms)
                            } else {
                                String::new()
                            };
                            info!(
                                "decode summary: {} total frames, {:.1} avg fps (GPU), {}x{}, stale_packets_skipped={}, device='{}'{}",
                                total_frames, avg_fps, width, height, stale_packets_skipped, device_name, arrival_info
                            );
                            last_summary_at = Instant::now();
                            summary_frame_counter = 0;
                            arrival_min_us = u64::MAX;
                            arrival_max_us = 0;
                            arrival_sum_us = 0;
                            arrival_count = 0;
                        }
                        continue; // skip software decode
                    }
                    Err(crate::gpu_decode::GpuDecodeError::Busy) => continue,
                    Err(crate::gpu_decode::GpuDecodeError::Unsupported(reason)) => {
                        // Every later frame would fail the same way; decode
                        // this one and the rest in software.
                        warn!("disabling GPU decode: {reason}");
                        gpu_decoder = None;
                    }
                    Err(crate::gpu_decode::GpuDecodeError::InvalidData(_)) => {
                        // Bad packet (e.g. config header) — skip to software decode
                        // for this packet, keep GPU decode active for the next one.
                        // The back slot is untouched and still holds its buffers.
                    }
                    Err(e) => {
                        // CUDA or nvJPEG error — count consecutive failures.
                        gpu_errors += 1;
                        if gpu_errors >= 3 {
                            warn!(
                                "nvJPEG failed {gpu_errors} times consecutively, \
                                 disabling GPU decode: {e}"
                            );
                            gpu_decoder = None;
                        } else {
                            warn!("nvJPEG decode error ({gpu_errors}/3): {e}");
                        }
                    }
                }
            }
        }

        // Software decode path (fallback or non-MJPEG formats)
        let packet_quant = if is_mjpeg { packet.data().and_then(JpegQuant::parse) } else { None };
        if let Err(error) = decoder.send_packet(&packet) {
            packet_errors += 1;
            if packet_errors <= 10 || packet_errors.is_multiple_of(50) {
                warn!(
                    "capture packet decode submission failed for '{}': {} (count={})",
                    device_name, error, packet_errors
                );
            }
            continue;
        }

        while decoder.receive_frame(&mut decoded).is_ok() {
            // Fill the back slot in place so its plane buffers are reused
            // rather than reallocated for every frame.
            let frame = frame_producer.back_slot();
            fill_frame_from_video(
                &decoded,
                &mut scaler,
                &mut scaled_frame,
                requested_fps > 60,
                frame,
            )
                .map_err(|error| CaptureError::Open(format!("failed to convert decoded frame: {error}")))?;
            // MJPEG decodes one frame per packet, so these are this frame's tables.
            frame.set_quant(packet_quant);
            let width = frame.width();
            let height = frame.height();
            total_frames += 1;

            if !logged_first_frame {
                logged_first_frame = true;
                info!(
                    "first decoded frame from '{}' => {}x{} {:?}",
                    device_name, width, height, frame.format()
                );
            }

            frame_producer.publish();
            let _ = event_proxy.send_event(AppEvent::FrameReady);

            stats_frame_counter += 1;
            summary_frame_counter += 1;
            let elapsed = last_stats_at.elapsed();
            if elapsed >= Duration::from_millis(300) {
                let fps = stats_frame_counter as f32 / elapsed.as_secs_f32();
                let _ = stats_tx.send(CaptureStats { fps, width, height });
                last_stats_at = Instant::now();
                stats_frame_counter = 0;
            }
            let summary_elapsed = last_summary_at.elapsed();
            if summary_elapsed >= Duration::from_secs(30) {
                let avg_fps = summary_frame_counter as f64 / summary_elapsed.as_secs_f64();
                let arrival_info = if arrival_count > 0 {
                    let avg_ms = (arrival_sum_us as f64 / arrival_count as f64) / 1000.0;
                    let min_ms = arrival_min_us as f64 / 1000.0;
                    let max_ms = arrival_max_us as f64 / 1000.0;
                    format!(", frame_arrival: avg={:.1}ms min={:.1}ms max={:.1}ms", avg_ms, min_ms, max_ms)
                } else {
                    String::new()
                };
                info!(
                    "decode summary: {} total frames, {:.1} avg fps (SW), {}x{}, packet_errors={}, stale_packets_skipped={}, device='{}'{}",
                    total_frames, avg_fps, width, height, packet_errors, stale_packets_skipped, device_name, arrival_info
                );
                last_summary_at = Instant::now();
                summary_frame_counter = 0;
                arrival_min_us = u64::MAX;
                arrival_max_us = 0;
                arrival_sum_us = 0;
                arrival_count = 0;
            }
        }
    }
}

/// Replaces `packet` with the newest packet for `stream_index` that the
/// device has already queued, without waiting for more. Returns how many
/// stale packets were skipped.
///
/// Without this, any stall (software MJPEG decode slower than the capture
/// rate, a slow first frame, the packets `avformat_find_stream_info` buffered
/// during startup) leaves a backlog in DirectShow's real-time buffer that is
/// then displayed in order, permanently adding the backlog's duration to the
/// latency. Up to `rtbufsize` (16 MB) of MJPEG is several hundred
/// milliseconds.
fn take_newest_queued_packet(
    input: &mut format::context::Input,
    stream_index: usize,
    packet: &mut Packet,
) -> u64 {
    let nonblock = ffmpeg::ffi::AVFMT_FLAG_NONBLOCK as std::ffi::c_int;
    // SAFETY: the context is valid for the lifetime of `input`, and only this
    // thread touches it. The dshow demuxer re-reads `flags` on every call.
    unsafe { (*input.as_mut_ptr()).flags |= nonblock };
    let mut skipped = 0;
    loop {
        let mut queued = Packet::empty();
        if queued.read(input).is_err() {
            // EAGAIN: nothing more is queued. Any real error resurfaces on
            // the next blocking read, which handles it.
            break;
        }
        if queued.stream() == stream_index {
            *packet = queued;
            skipped += 1;
        }
    }
    unsafe { (*input.as_mut_ptr()).flags &= !nonblock };
    skipped
}

fn fill_frame_from_video(
    frame: &Video,
    scaler: &mut Option<ScaleContext>,
    scaled_frame: &mut Video,
    prefer_yuvj422p: bool,
    out: &mut CaptureFrame,
) -> Result<(), String> {
    let width = frame.width();
    let height = frame.height();

    let format = match frame.format() {
        format::Pixel::NV12 => PixelFormat::Nv12,
        format::Pixel::YUVJ422P => PixelFormat::Yuvj422p,
        other => {
            let target = if prefer_yuvj422p {
                format::Pixel::YUVJ422P
            } else {
                format::Pixel::NV12
            };

            if scaler
                .as_ref()
                .map(|ctx| {
                    ctx.input().format != other
                        || ctx.input().width != width
                        || ctx.input().height != height
                        || ctx.output().format != target
                        || ctx.output().width != width
                        || ctx.output().height != height
                })
                .unwrap_or(true)
            {
                *scaler = Some(
                    ScaleContext::get(other, width, height, target, width, height, ScaleFlags::BILINEAR)
                        .map_err(|error| {
                            format!(
                                "unsupported pixel format {other:?} and failed to initialize converter to {target:?}: {error}"
                            )
                        })?,
                );
                *scaled_frame = Video::empty();
                warn!("converting decoder output from {other:?} to {target:?} for compatibility");
            }

            let Some(scale_ctx) = scaler.as_mut() else {
                return Err("scaler context unavailable".to_string());
            };

            scale_ctx
                .run(frame, scaled_frame)
                .map_err(|error| format!("failed to convert frame via swscale: {error}"))?;
            return extract_supported_frame(scaled_frame, out);
        }
    };

    extract_supported_frame_with_format(frame, format, width, height, out);
    Ok(())
}

fn extract_supported_frame(frame: &Video, out: &mut CaptureFrame) -> Result<(), String> {
    let width = frame.width();
    let height = frame.height();
    let format = match frame.format() {
        format::Pixel::NV12 => PixelFormat::Nv12,
        format::Pixel::YUVJ422P => PixelFormat::Yuvj422p,
        other => return Err(format!("unsupported converted pixel format: {other:?}")),
    };
    extract_supported_frame_with_format(frame, format, width, height, out);
    Ok(())
}

/// Copies the decoded planes into `out`, tightly packed. NV12 keeps its
/// interleaved UV plane as-is; the renderer uploads it as one two-channel
/// texture instead of splitting it on the CPU.
fn extract_supported_frame_with_format(
    frame: &Video,
    format: PixelFormat,
    width: u32,
    height: u32,
    out: &mut CaptureFrame,
) {
    let (width_px, height_px) = (width as usize, height as usize);
    let chroma_width = (width / 2) as usize;
    let (u_rows, u_row_bytes, v_rows) = match format {
        // Interleaved UV: half the rows, two bytes per chroma sample.
        PixelFormat::Nv12 => ((height / 2) as usize, chroma_width * 2, 0),
        PixelFormat::Yuvj422p => (height_px, chroma_width, height_px),
    };
    let (y_data, u_data, v_data) = out.reshape_cpu(
        width,
        height,
        format,
        width_px * height_px,
        u_row_bytes * u_rows,
        chroma_width * v_rows,
    );
    copy_plane(frame, 0, width_px, height_px, y_data);
    copy_plane(frame, 1, u_row_bytes, u_rows, u_data);
    if v_rows > 0 {
        copy_plane(frame, 2, chroma_width, v_rows, v_data);
    }
}

/// Build an ordered list of (width, height, fps) to try. Starts with the
/// requested settings, then appends common fallback tiers — halved fps at
/// the same resolution, then standard lower resolutions at 60/30 fps.
/// Duplicates of the original request are excluded.
fn resolution_fallback_tiers(width: u32, height: u32, fps: u32) -> Vec<(u32, u32, u32)> {
    let mut tiers: Vec<(u32, u32, u32)> = Vec::new();
    let mut push = |w, h, f| {
        if !tiers.contains(&(w, h, f)) {
            tiers.push((w, h, f));
        }
    };

    // Tier 0: exactly what the user asked for
    push(width, height, fps);

    // Tier 1: same resolution at half fps (e.g. 1080p120 -> 1080p60, 1080p60 -> 1080p30)
    if fps > 30 {
        push(width, height, fps / 2);
    }
    // Tier 2: same resolution at 30fps
    if fps != 30 {
        push(width, height, 30);
    }

    // Tier 3: standard fallback resolutions
    let fallbacks: &[(u32, u32)] = &[
        (1920, 1080),
        (1280, 720),
        (640, 480),
    ];
    for &(fw, fh) in fallbacks {
        if fw >= width && fh >= height {
            continue; // skip resolutions >= what we already tried
        }
        push(fw, fh, 60);
        push(fw, fh, 30);
    }

    tiers
}

fn pixel_format_attempts(requested_pixel_format: &str) -> Vec<Option<String>> {
    let mut attempts = Vec::new();
    let mut push_unique = |value: Option<&str>| {
        if attempts.iter().any(|existing: &Option<String>| existing.as_deref() == value) {
            return;
        }
        attempts.push(value.map(str::to_string));
    };

    push_unique(Some(requested_pixel_format));
    push_unique(Some("mjpeg"));
    push_unique(Some("nv12"));
    push_unique(Some("yuyv422"));
    push_unique(Some("uyvy422"));
    push_unique(Some("yuv420p"));
    push_unique(None);
    attempts
}

fn copy_plane(frame: &Video, plane: usize, row_bytes: usize, rows: usize, output: &mut [u8]) {
    let stride = frame.stride(plane);
    let source = frame.data(plane);

    if stride == row_bytes {
        output.copy_from_slice(&source[..row_bytes * rows]);
        return;
    }
    for (row, destination) in output.chunks_exact_mut(row_bytes).take(rows).enumerate() {
        let src_start = row * stride;
        destination.copy_from_slice(&source[src_start..src_start + row_bytes]);
    }
}

fn find_dshow_format() -> Option<ffmpeg::Format> {
    device::input::video().find(|format| match format {
        ffmpeg::Format::Input(input) => input.name() == "dshow",
        ffmpeg::Format::Output(_) => false,
    })
}

fn generate_test_frame(width: u32, height: u32, frame_index: u64, format: PixelFormat) -> CaptureFrame {
    let y_len = (width * height) as usize;
    let chroma_width = (width / 2) as usize;
    let chroma_height_420 = (height / 2) as usize;
    let chroma_height_422 = height as usize;
    let mut y_data = vec![0_u8; y_len];

    for y in 0..height as usize {
        for x in 0..width as usize {
            let xf = x as f32 / width as f32;
            let yf = y as f32 / height as f32;
            let phase = (frame_index as f32 * 0.04) + xf * PI * 2.0;
            let wave = ((phase.sin() * 0.5) + 0.5) * 80.0;
            let sweep = (((yf * 255.0) + frame_index as f32 * 2.0) as i32).rem_euclid(256) as u8;
            let bars = (((x * 8) / width as usize) * 28) as u8;
            y_data[y * width as usize + x] = sweep
                .saturating_div(2)
                .saturating_add(bars / 2)
                .saturating_add(wave as u8 / 2);
        }
    }

    match format {
        PixelFormat::Nv12 => {
            let mut uv_data = vec![0_u8; chroma_width * chroma_height_420 * 2];
            for y in 0..chroma_height_420 {
                for x in 0..chroma_width {
                    let index = (y * chroma_width + x) * 2;
                    let x_phase = ((x as f32 / chroma_width as f32) * PI * 2.0
                        + frame_index as f32 * 0.03)
                        .sin();
                    let y_phase = ((y as f32 / chroma_height_420 as f32) * PI * 2.0
                        + frame_index as f32 * 0.05)
                        .cos();
                    uv_data[index] = ((x_phase * 0.5 + 0.5) * 255.0) as u8;
                    uv_data[index + 1] = ((y_phase * 0.5 + 0.5) * 255.0) as u8;
                }
            }

            CaptureFrame::Cpu {
                width,
                height,
                format,
                y_data,
                u_data: uv_data,
                v_data: Vec::new(),
                quant: None,
            }
        }
        PixelFormat::Yuvj422p => {
            let mut u_data = vec![0_u8; chroma_width * chroma_height_422];
            let mut v_data = vec![0_u8; chroma_width * chroma_height_422];

            for y in 0..chroma_height_422 {
                for x in 0..chroma_width {
                    let index = y * chroma_width + x;
                    let x_phase = ((x as f32 / chroma_width as f32) * PI * 2.0
                        + frame_index as f32 * 0.06)
                        .sin();
                    let y_phase = ((y as f32 / chroma_height_422 as f32) * PI * 2.0
                        + frame_index as f32 * 0.02)
                        .cos();
                    u_data[index] = ((x_phase * 0.5 + 0.5) * 255.0) as u8;
                    v_data[index] = ((y_phase * 0.5 + 0.5) * 255.0) as u8;
                }
            }

            CaptureFrame::Cpu {
                width,
                height,
                format,
                y_data,
                u_data,
                v_data,
                quant: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_expected_plane_sizes_for_nv12() {
        let frame = generate_test_frame(1280, 720, 0, PixelFormat::Nv12);
        let CaptureFrame::Cpu { y_data, u_data, v_data, .. } = &frame else {
            panic!("expected Cpu frame");
        };
        assert_eq!(y_data.len(), 1280 * 720);
        assert_eq!(u_data.len(), 1280 * 720 / 2);
        assert!(v_data.is_empty());
    }

    #[test]
    fn generates_expected_plane_sizes_for_yuvj422p() {
        let frame = generate_test_frame(1280, 720, 0, PixelFormat::Yuvj422p);
        let CaptureFrame::Cpu { y_data, u_data, v_data, .. } = &frame else {
            panic!("expected Cpu frame");
        };
        assert_eq!(y_data.len(), 1280 * 720);
        assert_eq!(u_data.len(), 640 * 720);
        assert_eq!(v_data.len(), 640 * 720);
    }

    #[test]
    fn pixel_format_attempts_include_fallbacks_without_duplicates() {
        let attempts = pixel_format_attempts("nv12");
        let labels: Vec<_> = attempts.iter().map(|s| s.as_deref().unwrap_or("auto")).collect();
        assert!(labels.contains(&"nv12"));
        assert!(labels.contains(&"mjpeg"));
        assert!(labels.contains(&"auto"));
        let nv12_count = labels.iter().filter(|v| **v == "nv12").count();
        assert_eq!(nv12_count, 1);
    }
}

