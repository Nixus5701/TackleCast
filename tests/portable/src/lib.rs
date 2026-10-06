#[path = "../../../src/rtx_vsr.rs"] mod rtx_vsr;
#[path = "../../../src/settings.rs"] mod settings;
#[path = "../../../src/triple_buffer.rs"] mod triple_buffer;

#[cfg(test)]
mod shader_tests {
    #[test]
    fn rendering_shaders_parse_and_validate() {
        let mut checked = 0;
        for source in [include_str!("../../../src/render.rs"), include_str!("../../../src/render/vsr.rs")] {
            for fragment in source.split("r#\"").skip(1) {
                let shader = fragment.split("\"#").next().unwrap();
                if !shader.contains("@vertex") { continue; }
                checked += 1;
                let shader = format!("{}\n{}", include_str!("../../../src/render/image_adjustments.wgsl"), shader);
                let module = naga::front::wgsl::parse_str(&shader).unwrap();
                let (_, params) = module.types.iter().find(|(_, t)| t.name.as_deref() == Some("ImageParams")).unwrap();
                if let naga::TypeInner::Struct { span, ref members } = params.inner {
                    assert_eq!(span, 32);
                    assert_eq!(members.iter().map(|m| m.offset).collect::<Vec<_>>(), vec![0, 4, 8, 12, 16, 20, 24, 28]);
                } else { panic!("ImageParams must be a struct"); }
                naga::valid::Validator::new(naga::valid::ValidationFlags::all(),
                    naga::valid::Capabilities::all()).validate(&module).unwrap();
            }
        }
        assert_eq!(checked, 2);
    }
}
#[path = "../../../src/presentation.rs"] mod presentation;
#[path = "../../../src/frame_storage.rs"] mod frame_storage;

#[test]
fn executable_asset_decodes_for_owned_window_icon() {
    let icon = image::load_from_memory_with_format(include_bytes!("../../../assets/icon.ico"),
        image::ImageFormat::Ico).unwrap().into_rgba8();
    assert!(icon.width() >= 32 && icon.height() >= 32);
    assert_eq!(icon.as_raw().len(), (icon.width() * icon.height() * 4) as usize);
    assert!(icon.pixels().any(|p| p.0[3] != 0));
}
