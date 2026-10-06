param(
    [string]$FFmpegDir = $env:FFMPEG_DIR,
    [string]$RuntimeDirectory,
    [switch]$RunTests
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'Install Rust for Windows (MSVC, x64) from https://rustup.rs and open a new terminal.'
    }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path $vswhere) {
            $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
            if ($installation) {
                $devShell = Join-Path $installation 'Common7\Tools\Launch-VsDevShell.ps1'
                & $devShell -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
            }
        }
    }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
        throw 'Install Visual Studio 2022 Build Tools with Desktop development with C++ and a Windows 10/11 SDK.'
    }
    if (-not $FFmpegDir) {
        throw 'Specify an FFmpeg 8 shared development build: .\Build-RTX.cmd -FFmpegDir C:\ffmpeg . The directory must contain include, lib, and bin.'
    }
    $env:FFMPEG_DIR = (Resolve-Path $FFmpegDir).Path
    foreach ($part in @('include\libavcodec\avcodec.h', 'lib', 'bin')) {
        if (-not (Test-Path (Join-Path $env:FFMPEG_DIR $part))) {
            throw "FFMPEG_DIR is missing $part. A runtime-only TackleCast folder is insufficient to compile. See BUILD.md."
        }
    }
    if (-not $env:LIBCLANG_PATH) {
        $clang = Join-Path $env:ProgramFiles 'LLVM\bin'
        if (Test-Path (Join-Path $clang 'libclang.dll')) { $env:LIBCLANG_PATH = $clang }
    }
    if (-not $env:LIBCLANG_PATH -or -not (Test-Path (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) {
        throw 'Install LLVM with libclang, or set LIBCLANG_PATH to the directory containing libclang.dll.'
    }
    $env:PATH = "$(Join-Path $env:FFMPEG_DIR 'bin');$env:PATH"
    if ($RunTests) {
        & cargo test --locked --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed.' }
    }
    & cargo build --release --locked --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Compilation failed; preserve the first compiler error for diagnosis.' }
    $destination = Join-Path (Get-Location) 'dist\TackleCast-RTX'
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    if ($RuntimeDirectory) {
        # Keep the existing installation untouched. Copy only CUDA runtime DLLs;
        # FFmpeg DLLs below must match the headers/libs used for this build.
        foreach ($pattern in @('nvjpeg*.dll', 'cudart*.dll')) {
            Get-ChildItem -Path $RuntimeDirectory -Filter $pattern -File |
                Copy-Item -Destination $destination -Force
        }
    }
    Get-ChildItem -Path (Join-Path $env:FFMPEG_DIR 'bin') -Filter '*.dll' -File |
        Copy-Item -Destination $destination -Force
    Copy-Item 'target\x86_64-pc-windows-msvc\release\tacklecast.exe' (Join-Path $destination 'TackleCast.exe') -Force
    $assetDestination = Join-Path $destination 'assets'
    New-Item -ItemType Directory -Force -Path $assetDestination | Out-Null
    Copy-Item 'assets\*' $assetDestination -Recurse -Force
    Copy-Item 'LICENSE' $destination -Force
    Copy-Item 'docs\RTX-SUPER-RESOLUTION.md' $destination -Force
    Copy-Item 'docs\LATENCY-UPDATE.md' $destination -Force
    Copy-Item 'docs\IMAGE-ADJUSTMENTS.md' $destination -Force
    Copy-Item 'docs\ICON-FIX.md' $destination -Force
    Write-Host "Build complete: $destination\TackleCast.exe"
    Write-Host 'For GPU MJPEG decoding, include nvjpeg64_13.dll and cudart64_13.dll from the original TackleCast package or CUDA installation.'
    Write-Host 'Enable NVIDIA RTX Video Super Resolution, then Esc > Video > Request Super Resolution in TackleCast.'
} finally {
    Pop-Location
}
