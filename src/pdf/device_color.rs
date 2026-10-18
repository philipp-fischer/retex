//! Default unmanaged DeviceCMYK screen profile. PDF device colors do not prescribe
//! one RGB conversion. This 16-corner profile preserves the recorded independent
//! Python/Poppler display policy; decoded source sample observations stay separate.
//! Calibration and held-out vector/image colors: tests/fixtures/device-cmyk-python.json.
//! PDF Reference 1.7 section 4.5.3 defines device colors as device dependent.
pub(super) const PROFILE: &str = "retex-device-cmyk16-v1";
const CORNERS: [[f64; 3]; 16] = [
    [1., 1., 1.],
    [0.1373, 0.1216, 0.1255],
    [1., 0.9490, 0.],
    [0.1098, 0.1020, 0.],
    [0.9255, 0., 0.5490],
    [0.1412, 0., 0.],
    [0.9294, 0.1098, 0.1412],
    [0.1333, 0., 0.],
    [0., 0.6784, 0.9373],
    [0., 0.0588, 0.1412],
    [0., 0.6510, 0.3137],
    [0., 0.0745, 0.],
    [0.1804, 0.1922, 0.5725],
    [0., 0., 0.0078],
    [0.2118, 0.2119, 0.2235],
    [0., 0., 0.],
];
fn convert(components: [u8; 4]) -> [f64; 3] {
    let components = components.map(|value| value as f64 / 255.);
    let mut rgb = [0.; 3];
    for (corner, color) in CORNERS.iter().enumerate() {
        let mut weight = 1.;
        for (component, value) in components.iter().enumerate() {
            weight *= if corner & (1 << (3 - component)) != 0 {
                *value
            } else {
                1. - value
            };
        }
        for channel in 0..3 {
            rgb[channel] += weight * color[channel];
        }
    }
    rgb.map(|value| value.clamp(0., 1.) * 255.)
}
// Independent raster observations truncate the device conversion. Vector
// color operands use a different quantization and are checked separately.
pub(super) fn cmyk_image_rgb(components: [u8; 4]) -> [u8; 3] {
    convert(components).map(|value| value as u8)
}
#[cfg(test)]
mod tests {
    #[test]
    fn independent_python_pdf_corners_and_held_out_mixed_colors() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/device-cmyk-python.json"))
                .unwrap();
        assert_eq!(fixture["calibration_count"], 16);
        assert_eq!(fixture["samples"].as_array().unwrap().len(), 256);
        let coefficients: [[f64; 3]; 16] =
            serde_json::from_value(fixture["coefficients"].clone()).unwrap();
        assert_eq!(super::CORNERS, coefficients);
        for sample in fixture["samples"].as_array().unwrap() {
            let cmyk: [u8; 4] = serde_json::from_value(sample["cmyk"].clone()).unwrap();
            let rgb: [u8; 3] = serde_json::from_value(sample["rgb"].clone()).unwrap();
            assert_eq!(
                super::convert(cmyk).map(|v| v.round() as u8),
                rgb,
                "vector CMYK {cmyk:?}"
            );
            let image: [u8; 3] = serde_json::from_value(sample["image_rgb"].clone()).unwrap();
            assert_eq!(super::cmyk_image_rgb(cmyk), image, "image CMYK {cmyk:?}");
        }
    }
}
