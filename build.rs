fn main() {
    println!("cargo:rerun-if-changed=native/rtx_vsr.cpp");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        if std::env::var_os("CARGO_FEATURE_RTX_VSR").is_some() {
            cc::Build::new()
                .cpp(true)
                .file("native/rtx_vsr.cpp")
                .std("c++17")
                .flag_if_supported("/EHsc")
                .define("NOMINMAX", None)
                .define("WIN32_LEAN_AND_MEAN", None)
                .compile("tacklecast_rtx_vsr");
            println!("cargo:rustc-link-lib=d3d11");
            println!("cargo:rustc-link-lib=dxgi");
        }
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().unwrap();
    }
}
