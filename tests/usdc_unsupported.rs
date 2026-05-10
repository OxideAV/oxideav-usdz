//! USDZ archive whose only Default Layer is binary `.usdc` →
//! decoder should surface `Error::Unsupported` with a hint to
//! convert via `usdcat`.

mod common;

use oxideav_usdz::{error::Error, UsdzDecoder};

// Just the `PXR-USDC` magic — the parser bails before looking at
// the rest of the binary crate file.
const FAKE_USDC: &[u8] = b"PXR-USDC\0\0\0\0\0\0\0\0";

#[test]
fn binary_usdc_returns_unsupported() {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "default.usdc",
        payload: FAKE_USDC,
    }]);
    let err = UsdzDecoder::new()
        .decode_bytes(&usdz)
        .expect_err("decode_bytes should error on .usdc default layer");
    match err {
        Error::Unsupported(msg) => {
            assert!(
                msg.contains("usdc") || msg.contains("USDC") || msg.contains("crate"),
                "expected unsupported message to mention usdc/crate, got: {msg}"
            );
            assert!(
                msg.contains("usdcat") || msg.contains("usda"),
                "expected message to suggest a workaround, got: {msg}"
            );
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}
