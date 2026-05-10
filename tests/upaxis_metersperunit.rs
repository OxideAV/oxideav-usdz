//! USD `upAxis = "Z"` + `metersPerUnit = 0.001` should map onto
//! `Scene3D::up_axis = PosZ` and `Scene3D::unit = Millimetres`.

mod common;

use oxideav_mesh3d::{Axis, Unit};
use oxideav_usdz::UsdzDecoder;

const Z_UP_MM: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
    upAxis = "Z"
    metersPerUnit = 0.001
)
def Xform "Root" {
}
"#;

#[test]
fn z_up_millimetres() {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: Z_UP_MM.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    assert_eq!(scene.up_axis, Axis::PosZ);
    assert_eq!(scene.unit, Unit::Millimetres);
}

const Y_UP_INCHES: &str = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 0.0254
)
def Xform "Root" {
}
"#;

#[test]
fn y_up_inches() {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: Y_UP_INCHES.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    assert_eq!(scene.up_axis, Axis::PosY);
    assert_eq!(scene.unit, Unit::Inches);
}
