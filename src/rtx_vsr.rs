//! Optional NVIDIA D3D11 video-processor request, fed by TackleCast's D3D12
//! renderer. No desktop capture, CPU frame readback, or proprietary SDK DLLs.
//! See docs/RTX-SUPER-RESOLUTION.md for synchronization and validation notes.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dimensions {
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}

impl Dimensions {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.input_width == 0 || self.input_height < 360
            || self.input_width > 2560 || self.input_height > 1440
            || self.input_width % 2 != 0 || self.input_height % 2 != 0
        {
            return Err("Use an even-sized capture between 360p and 1440p");
        }
        if self.output_width < self.input_width || self.output_height < self.input_height {
            return Err("Enlarge the window to at least the capture resolution");
        }
        if self.output_width > 8192 || self.output_height > 8192 {
            return Err("Output exceeds the Super Resolution size limit");
        }
        Ok(self)
    }

    pub fn input_pitch(self) -> u32 { (self.input_width * 4 + 255) & !255 }
    pub fn output_pitch(self) -> u32 { (self.output_width * 4 + 255) & !255 }
}

#[cfg(all(windows, feature = "rtx-vsr"))]
mod native {
    use super::Dimensions;
    use std::ffi::{c_char, c_void, CStr};
    use std::ptr::NonNull;
    use wgpu::hal::api::Dx12;
    use windows::core::Interface;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    extern "C" {
        fn tc_vsr_create(device: *mut c_void, queue: *mut c_void,
            input: *mut c_void, output: *mut c_void,
            iw: u32, ih: u32, ow: u32, oh: u32, ip: u32, op: u32,
            error: *mut c_char, capacity: usize) -> *mut c_void;
        fn tc_vsr_process(bridge: *mut c_void, error: *mut c_char, capacity: usize) -> i32;
        fn tc_vsr_destroy(bridge: *mut c_void) -> bool;
    }

    pub struct Bridge {
        pointer: NonNull<c_void>,
        pub input_buffer: wgpu::Buffer,
        pub output_buffer: wgpu::Buffer,
    }

    impl Bridge {
        pub fn new(device: &wgpu::Device, queue: &wgpu::Queue,
                   d: Dimensions) -> Result<Self, String> {
            let mut error = [0 as c_char; 256];
            // SAFETY: all objects belong to this same DX12 device. The native
            // object AddRefs every COM pointer before these HAL guards expire.
            // The renderer serializes native calls with its queue submissions.
            let created = unsafe {
                device.as_hal::<Dx12, _, _>(|hal| {
                    let hal = hal.ok_or_else(|| "Direct3D 12 unavailable".to_owned())?;
                    let input = buffer(hal.raw_device(), u64::from(d.input_pitch()) * u64::from(d.input_height))?;
                    let output = buffer(hal.raw_device(), u64::from(d.output_pitch()) * u64::from(d.output_height))?;
                    let pointer = NonNull::new(tc_vsr_create(
                        hal.raw_device().as_raw(), hal.raw_queue().as_raw(),
                        input.as_raw(), output.as_raw(),
                        d.input_width, d.input_height, d.output_width, d.output_height,
                        d.input_pitch(), d.output_pitch(), error.as_mut_ptr(), error.len()))
                        .ok_or_else(|| message(&error, "Video bridge unavailable"))?;
                    Ok::<_, String>((pointer, input, output))
                })
            }?;
            let (pointer, input, output) = created;
            let input_buffer = wrap(device, input, u64::from(d.input_pitch()) * u64::from(d.input_height));
            let output_buffer = wrap(device, output, u64::from(d.output_pitch()) * u64::from(d.output_height));
            // Initialize through wgpu before any native writes. This marks the
            // entire buffers initialized so a later read never clears VSR data.
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.clear_buffer(&input_buffer, 0, None);
            encoder.clear_buffer(&output_buffer, 0, None);
            queue.submit([encoder.finish()]);
            Ok(Self { pointer, input_buffer, output_buffer })
        }

        pub fn process(&mut self) -> Result<bool, String> {
            let mut error = [0 as c_char; 256];
            // SAFETY: this uniquely owned pointer is valid until Drop. Calls
            // occur on the render thread, never concurrently with wgpu submit.
            match unsafe { tc_vsr_process(self.pointer.as_ptr(), error.as_mut_ptr(), error.len()) } {
                0 => Ok(true),
                1 => Ok(false),
                _ => Err(message(&error, "Video processing failed")),
            }
        }
    }

    impl Drop for Bridge {
        fn drop(&mut self) {
            // SAFETY: destroys exactly once, after waiting for native GPU work.
            if !unsafe { tc_vsr_destroy(self.pointer.as_ptr()) } {
                tracing::error!("RTX video bridge teardown timed out; GPU resources retained until exit");
            }
        }
    }

    unsafe fn buffer(device: &ID3D12Device, size: u64) -> Result<ID3D12Resource, String> {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT, CreationNodeMask: 1, VisibleNodeMask: 1,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER, Width: size, Height: 1,
            DepthOrArraySize: 1, MipLevels: 1, Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR, ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc,
            D3D12_RESOURCE_STATE_COMMON, None, &mut resource).map_err(|e| e.to_string())?;
        resource.ok_or_else(|| "D3D12 buffer creation returned no resource".to_owned())
    }

    fn wrap(device: &wgpu::Device, resource: ID3D12Resource, size: u64) -> wgpu::Buffer {
        // SAFETY: matching device, description and ownership; copies on the
        // same queue use COMMON-state promotion/decay for these buffers.
        unsafe {
            device.create_buffer_from_hal::<Dx12>(
                wgpu::hal::dx12::Device::buffer_from_raw(resource, size),
                &wgpu::BufferDescriptor { label: Some("rtx-vsr-interop-buffer"), size,
                    usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false })
        }
    }

    fn message(buffer: &[c_char], fallback: &str) -> String {
        // Native snprintf is bounded; buffers begin zero-filled.
        let message = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
        if message.is_empty() { fallback.to_owned() } else { message.into_owned() }
    }
}

#[cfg(all(windows, feature = "rtx-vsr"))]
pub use native::Bridge;

#[cfg(test)]
mod tests {
    use super::*;
    fn hd() -> Dimensions {
        Dimensions { input_width: 1920, input_height: 1080,
                     output_width: 1920, output_height: 1080 }
    }
    #[test]
    fn native_resolution_cleanup_is_allowed() { assert!(hd().validate().is_ok()); }
    #[test]
    fn enlargement_is_allowed() {
        assert!(Dimensions { output_width: 2560, output_height: 1440, ..hd() }.validate().is_ok());
    }
    #[test]
    fn downscaling_and_invalid_nv12_are_rejected() {
        assert!(Dimensions { output_width: 1280, ..hd() }.validate().is_err());
        assert!(Dimensions { input_width: 1919, ..hd() }.validate().is_err());
        assert!(Dimensions { input_height: 0, ..hd() }.validate().is_err());
        assert!(Dimensions { input_width: u32::MAX, ..hd() }.validate().is_err());
        assert!(Dimensions { output_height: 8193, ..hd() }.validate().is_err());
    }
    #[test]
    fn copy_rows_are_aligned_without_losing_pixels() {
        let d = Dimensions { input_width: 1922, output_width: 2562, ..hd() };
        assert!(d.validate().is_ok());
        assert_eq!(d.input_pitch(), 7936);
        assert_eq!(d.output_pitch(), 10496);
        assert_eq!(d.input_pitch() % 256, 0);
        assert!(d.input_pitch() >= d.input_width * 4);
    }
}
