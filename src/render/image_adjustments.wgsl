// Shared by direct video and the final VSR display. Not applied to VSR input.
struct ImageParams {
    brightness: f32, contrast: f32, saturation: f32, gamma: f32,
    hue_cos: f32, hue_sin: f32, vsr_detail: f32, padding: f32,
};
fn adjust_color(input: vec3<f32>, p: ImageParams) -> vec3<f32> {
    // Preserve the previous output exactly at neutral settings.
    if p.brightness == 0.0 && p.contrast == 1.0 && p.saturation == 1.0 &&
       p.gamma == 1.0 && p.hue_cos == 1.0 && p.hue_sin == 0.0 {
        return input;
    }
    var rgb = input;
    if p.saturation != 1.0 || p.hue_cos != 1.0 || p.hue_sin != 0.0 {
        let y = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let u = (rgb.b - y) / 1.8556;
        let v = (rgb.r - y) / 1.5748;
        let ru = (u * p.hue_cos - v * p.hue_sin) * p.saturation;
        let rv = (u * p.hue_sin + v * p.hue_cos) * p.saturation;
        let red = y + 1.5748 * rv;
        let blue = y + 1.8556 * ru;
        rgb = vec3<f32>(red, (y - 0.2126 * red - 0.0722 * blue) / 0.7152, blue);
    }
    rgb = clamp((rgb - vec3<f32>(0.5)) * p.contrast +
                vec3<f32>(0.5 + p.brightness), vec3<f32>(0.0), vec3<f32>(1.0));
    if p.gamma != 1.0 { rgb = pow(rgb, vec3<f32>(1.0 / p.gamma)); }
    return rgb;
}
