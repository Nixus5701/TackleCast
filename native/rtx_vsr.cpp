// Copyright (c) 2026 TackleCast contributors. MIT, see ../LICENSE.
// GPU-only D3D12 -> D3D11 VideoProcessor -> D3D12 bridge.
// All entry points run on the render thread, between wgpu queue submissions.
#include <windows.h>
#include <d3d11_4.h>
#include <d3d12.h>
#include <dxgi1_6.h>
#include <wrl/client.h>
#include <array>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <stdexcept>

using Microsoft::WRL::ComPtr;

namespace {
struct Failure { HRESULT hr; const char* operation; };
void check(HRESULT hr, const char* op) { if (FAILED(hr)) throw Failure{hr, op}; }
struct Handle {
    HANDLE value = nullptr;
    ~Handle() { if (value) CloseHandle(value); }
};
struct Processor {
    ComPtr<ID3D11VideoProcessorEnumerator> enumerator;
    ComPtr<ID3D11VideoProcessor> processor;
    ComPtr<ID3D11VideoProcessorInputView> input;
    ComPtr<ID3D11VideoProcessorOutputView> output;
};
struct Slot {
    std::array<ComPtr<ID3D12CommandAllocator>, 2> allocators;
    std::array<ComPtr<ID3D12GraphicsCommandList>, 2> lists;
    UINT64 completion = 0;
};
struct Bridge {
    ComPtr<ID3D12Device> d12;
    ComPtr<ID3D12CommandQueue> queue;
    ComPtr<ID3D11Device5> d11;
    ComPtr<ID3D11DeviceContext4> context;
    ComPtr<ID3D11VideoDevice> video;
    ComPtr<ID3D11VideoContext1> video_context;
    ComPtr<ID3D12Resource> input_buffer, output_buffer;
    ComPtr<ID3D11Texture2D> rgb_input, nv12, rgb_output;
    ComPtr<ID3D12Resource> shared_input, shared_output;
    ComPtr<ID3D12Fence> to11, to12, done;
    ComPtr<ID3D11Fence> to11_view, to12_view;
    Processor conversion, enhancement;
    std::array<Slot, 3> slots;
    UINT64 serial = 0;
    bool poisoned = false;

    void shared_texture(UINT w, UINT h, ComPtr<ID3D11Texture2D>& texture,
                        ComPtr<ID3D12Resource>& resource) {
        D3D11_TEXTURE2D_DESC td = {};
        td.Width = w; td.Height = h; td.MipLevels = td.ArraySize = 1;
        td.Format = DXGI_FORMAT_B8G8R8A8_UNORM; td.SampleDesc.Count = 1;
        td.Usage = D3D11_USAGE_DEFAULT;
        td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        td.MiscFlags = D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE;
        check(d11->CreateTexture2D(&td, nullptr, &texture), "Create shared texture");
        ComPtr<IDXGIResource1> dxgi;
        check(texture.As(&dxgi), "Query shared texture");
        Handle handle;
        check(dxgi->CreateSharedHandle(nullptr, DXGI_SHARED_RESOURCE_READ |
            DXGI_SHARED_RESOURCE_WRITE, nullptr, &handle.value), "Share texture");
        check(d12->OpenSharedHandle(handle.value, IID_PPV_ARGS(&resource)), "Open shared texture");
    }

    void shared_fence(ComPtr<ID3D12Fence>& f12, ComPtr<ID3D11Fence>& f11) {
        check(d12->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(&f12)), "Create shared fence");
        Handle handle;
        check(d12->CreateSharedHandle(f12.Get(), nullptr, GENERIC_ALL, nullptr,
                                     &handle.value), "Share fence");
        check(d11->OpenSharedFence(handle.value, IID_PPV_ARGS(&f11)), "Open shared fence");
    }

    void processor(Processor& p, ID3D11Texture2D* input, ID3D11Texture2D* output,
                   UINT iw, UINT ih, UINT ow, UINT oh, bool enhance) {
        D3D11_VIDEO_PROCESSOR_CONTENT_DESC cd = {};
        cd.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
        cd.InputWidth = iw; cd.InputHeight = ih;
        cd.OutputWidth = ow; cd.OutputHeight = oh;
        cd.InputFrameRate = cd.OutputFrameRate = {60, 1};
        cd.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
        check(video->CreateVideoProcessorEnumerator(&cd, &p.enumerator), "Create video enumerator");
        auto require_format = [&](DXGI_FORMAT format, UINT flag) {
            UINT support = 0;
            check(p.enumerator->CheckVideoProcessorFormat(format, &support), "Check video format");
            if (!(support & flag)) throw Failure{E_NOTIMPL, "Required video format unsupported"};
        };
        require_format(enhance ? DXGI_FORMAT_NV12 : DXGI_FORMAT_B8G8R8A8_UNORM,
                       D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT);
        require_format(enhance ? DXGI_FORMAT_B8G8R8A8_UNORM : DXGI_FORMAT_NV12,
                       D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT);
        check(video->CreateVideoProcessor(p.enumerator.Get(), 0, &p.processor), "Create video processor");
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC iv = {};
        iv.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
        check(video->CreateVideoProcessorInputView(input, p.enumerator.Get(), &iv, &p.input), "Create video input");
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC ov = {};
        ov.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
        check(video->CreateVideoProcessorOutputView(output, p.enumerator.Get(), &ov, &p.output), "Create video output");
        RECT src{0, 0, static_cast<LONG>(iw), static_cast<LONG>(ih)};
        RECT dst{0, 0, static_cast<LONG>(ow), static_cast<LONG>(oh)};
        video_context->VideoProcessorSetStreamFrameFormat(p.processor.Get(), 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
        video_context->VideoProcessorSetStreamSourceRect(p.processor.Get(), 0, TRUE, &src);
        video_context->VideoProcessorSetStreamDestRect(p.processor.Get(), 0, TRUE, &dst);
        video_context->VideoProcessorSetOutputTargetRect(p.processor.Get(), TRUE, &dst);
        video_context->VideoProcessorSetStreamAutoProcessingMode(p.processor.Get(), 0, FALSE);
        // Existing TackleCast shader provides full-range SDR RGB. Convert to
        // studio-range BT.709 NV12 explicitly, then back to full-range RGB.
        video_context->VideoProcessorSetStreamColorSpace1(p.processor.Get(), 0,
            enhance ? DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709 : DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
        video_context->VideoProcessorSetOutputColorSpace1(p.processor.Get(),
            enhance ? DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 : DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
        if (enhance) {
            // NVIDIA PPE stream-extension ABI, also documented by Chromium's
            // ToggleNvidiaVpSuperResolution. S_OK confirms request acceptance
            // only. The NVIDIA App reports actual model activation.
            const GUID ppe = {0xd43ce1b3, 0x1f4b, 0x48ac,
                {0xba, 0xee, 0xc3, 0xc2, 0x53, 0x75, 0xe6, 0xf7}};
            struct { UINT version, method, enable; } request{1, 2, 1};
            HRESULT hr = video_context->VideoProcessorSetStreamExtension(
                p.processor.Get(), 0, &ppe, sizeof(request), &request);
            if (hr != S_OK) throw Failure{FAILED(hr) ? hr : E_NOTIMPL, "NVIDIA Super Resolution request rejected"};
        }
    }

    static void barrier(ID3D12GraphicsCommandList* list, ID3D12Resource* resource,
                        D3D12_RESOURCE_STATES before, D3D12_RESOURCE_STATES after) {
        D3D12_RESOURCE_BARRIER b = {};
        b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
        b.Transition = {resource, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, before, after};
        list->ResourceBarrier(1, &b);
    }

    void record_copy(ID3D12GraphicsCommandList* list, bool into11, UINT w, UINT h, UINT pitch) {
        auto* texture = into11 ? shared_input.Get() : shared_output.Get();
        auto* buffer = into11 ? input_buffer.Get() : output_buffer.Get();
        D3D12_TEXTURE_COPY_LOCATION tex = {};
        tex.pResource = texture; tex.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
        D3D12_TEXTURE_COPY_LOCATION buf = {};
        buf.pResource = buffer; buf.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT;
        buf.PlacedFootprint.Footprint = {DXGI_FORMAT_B8G8R8A8_UNORM, w, h, 1, pitch};
        // Textures return to COMMON before crossing API boundaries. Buffers
        // decay to COMMON after ExecuteCommandLists (including wgpu's submits).
        auto state = into11 ? D3D12_RESOURCE_STATE_COPY_DEST : D3D12_RESOURCE_STATE_COPY_SOURCE;
        barrier(list, texture, D3D12_RESOURCE_STATE_COMMON, state);
        if (into11) list->CopyTextureRegion(&tex, 0, 0, 0, &buf, nullptr);
        else list->CopyTextureRegion(&buf, 0, 0, 0, &tex, nullptr);
        barrier(list, texture, state, D3D12_RESOURCE_STATE_COMMON);
        check(list->Close(), "Close copy commands");
    }

    Bridge(ID3D12Device* device, ID3D12CommandQueue* q,
           ID3D12Resource* in, ID3D12Resource* out,
           UINT iw, UINT ih, UINT ow, UINT oh, UINT ip, UINT op) {
        d12 = device; queue = q; input_buffer = in; output_buffer = out;
        ComPtr<IDXGIFactory4> factory;
        check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "Create DXGI factory");
        ComPtr<IDXGIAdapter1> adapter;
        check(factory->EnumAdapterByLuid(d12->GetAdapterLuid(), IID_PPV_ARGS(&adapter)), "Find rendering adapter");
        DXGI_ADAPTER_DESC1 desc = {};
        check(adapter->GetDesc1(&desc), "Read adapter");
        if (desc.VendorId != 0x10de) throw Failure{E_NOTIMPL, "RTX Super Resolution requires an NVIDIA adapter"};
        ComPtr<ID3D11Device> base;
        ComPtr<ID3D11DeviceContext> base_context;
        const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
        UINT flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        wchar_t debug[2] = {};
        if (GetEnvironmentVariableW(L"TACKLECAST_VSR_DEBUG", debug, 2) && debug[0] == L'1')
            flags |= D3D11_CREATE_DEVICE_DEBUG;
        check(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, flags,
            levels, 2, D3D11_SDK_VERSION, &base, nullptr, &base_context), "Create D3D11 video device");
        check(base.As(&d11), "D3D11 shared-fence device unavailable");
        check(base_context.As(&context), "D3D11 shared-fence context unavailable");
        check(base.As(&video), "Query video device");
        check(base_context.As(&video_context), "Query video context");
        shared_texture(iw, ih, rgb_input, shared_input);
        shared_texture(ow, oh, rgb_output, shared_output);
        D3D11_TEXTURE2D_DESC td = {};
        td.Width = iw; td.Height = ih; td.MipLevels = td.ArraySize = 1;
        td.Format = DXGI_FORMAT_NV12; td.SampleDesc.Count = 1;
        td.Usage = D3D11_USAGE_DEFAULT; td.BindFlags = D3D11_BIND_RENDER_TARGET;
        check(d11->CreateTexture2D(&td, nullptr, &nv12), "Create NV12 intermediate");
        processor(conversion, rgb_input.Get(), nv12.Get(), iw, ih, iw, ih, false);
        processor(enhancement, nv12.Get(), rgb_output.Get(), iw, ih, ow, oh, true);
        shared_fence(to11, to11_view);
        shared_fence(to12, to12_view);
        check(d12->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&done)), "Create completion fence");
        // Command lists are immutable for this source/destination size.
        // Never reset/reuse a slot until its GPU completion is observed.
        for (auto& slot : slots) {
            for (int i = 0; i < 2; ++i) {
                check(d12->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT,
                      IID_PPV_ARGS(&slot.allocators[i])), "Create copy allocator");
                check(d12->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT,
                    slot.allocators[i].Get(), nullptr, IID_PPV_ARGS(&slot.lists[i])), "Create copy list");
                record_copy(slot.lists[i].Get(), i == 0, i == 0 ? iw : ow,
                            i == 0 ? ih : oh, i == 0 ? ip : op);
            }
        }
    }

    HRESULT process() {
        if (poisoned) throw Failure{E_FAIL, "Video bridge needs reinitialization"};
        check(d12->GetDeviceRemovedReason(), "D3D12 device removed");
        const UINT64 completed = done->GetCompletedValue();
        Slot* available = nullptr;
        for (auto& slot : slots) if (slot.completion <= completed) { available = &slot; break; }
        if (!available) return S_FALSE; // bypass this frame instead of adding a CPU wait
        const UINT64 value = ++serial;
        ID3D12CommandList* first[] = {available->lists[0].Get()};
        queue->ExecuteCommandLists(1, first);
        check(queue->Signal(to11.Get(), value), "Signal D3D12 input");
        check(context->Wait(to11_view.Get(), value), "Wait for D3D12 input");
        auto blit = [&](Processor& p) {
            D3D11_VIDEO_PROCESSOR_STREAM stream = {};
            stream.Enable = TRUE; stream.pInputSurface = p.input.Get();
            return video_context->VideoProcessorBlt(p.processor.Get(), p.output.Get(), 0, 1, &stream);
        };
        HRESULT converted = blit(conversion);
        HRESULT enhanced = FAILED(converted) ? converted : blit(enhancement);
        // Establish completion even when a blit fails. Never enqueue a D3D12
        // wait for a D3D11 signal that was not successfully submitted.
        check(context->Signal(to12_view.Get(), value), "Signal D3D11 output");
        context->Flush();
        check(queue->Wait(to12.Get(), value), "Wait for D3D11 output");
        if (SUCCEEDED(enhanced)) {
            ID3D12CommandList* second[] = {available->lists[1].Get()};
            queue->ExecuteCommandLists(1, second);
        }
        check(queue->Signal(done.Get(), value), "Signal copy completion");
        available->completion = value;
        check(enhanced, "Video processing failed");
        return S_OK;
    }

    bool idle() {
        // Only teardown/reconfiguration can wait on the CPU. If a driver hangs,
        // retain this object rather than freeing resources still in GPU use.
        // A removed device no longer executes work and can be released normally.
        if (FAILED(d12->GetDeviceRemovedReason())) return true;
        context->Flush();
        Handle event;
        event.value = CreateEventW(nullptr, FALSE, FALSE, nullptr);
        if (!event.value) return false;
        const UINT64 value = ++serial;
        if (FAILED(context->Signal(to12_view.Get(), value))) return false;
        context->Flush();
        if (FAILED(queue->Wait(to12.Get(), value))) return false;
        if (FAILED(queue->Signal(done.Get(), value))) return false;
        if (FAILED(done->SetEventOnCompletion(value, event.value))) return false;
        return WaitForSingleObject(event.value, 5000) == WAIT_OBJECT_0;
    }
};

void error_text(char* out, size_t capacity, const Failure& e) {
    if (out && capacity) std::snprintf(out, capacity, "%s (0x%08lX)", e.operation,
                                     static_cast<unsigned long>(e.hr));
}
} // namespace

extern "C" {
// Pointers are borrowed at the boundary; Bridge retains its own COM references.
void* tc_vsr_create(void* device, void* queue, void* input, void* output,
                    UINT iw, UINT ih, UINT ow, UINT oh, UINT ip, UINT op,
                    char* error, size_t capacity) noexcept {
    try {
        if (!device || !queue || !input || !output || !iw || !ih || !ow || !oh ||
            iw > 2560 || ih < 360 || ih > 1440 || ow < iw || oh < ih ||
            ow > 8192 || oh > 8192 || (iw & 1) || (ih & 1) ||
            ip < iw * 4 || op < ow * 4 || (ip & 255) || (op & 255))
            throw Failure{E_INVALIDARG, "Invalid video bridge dimensions"};
        return new Bridge(static_cast<ID3D12Device*>(device), static_cast<ID3D12CommandQueue*>(queue),
            static_cast<ID3D12Resource*>(input), static_cast<ID3D12Resource*>(output),
            iw, ih, ow, oh, ip, op);
    } catch (const Failure& e) { error_text(error, capacity, e); }
      catch (...) { error_text(error, capacity, {E_FAIL, "Could not initialize video bridge"}); }
    return nullptr;
}
// 0 = processed; 1 = busy, bypass; negative = failed, disable until retried.
int tc_vsr_process(void* bridge, char* error, size_t capacity) noexcept {
    if (!bridge) return -1;
    auto* b = static_cast<Bridge*>(bridge);
    try { return b->process() == S_OK ? 0 : 1; }
    catch (const Failure& e) { b->poisoned = true; error_text(error, capacity, e); }
    catch (...) { b->poisoned = true; error_text(error, capacity, {E_FAIL, "Video bridge exception"}); }
    return -1;
}
// False means a hung driver required retaining resources until process exit.
bool tc_vsr_destroy(void* bridge) noexcept {
    auto* b = static_cast<Bridge*>(bridge);
    if (!b) return true;
    if (!b->idle()) return false;
    delete b;
    return true;
}
}
