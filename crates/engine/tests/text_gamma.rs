//! Edit › Color Settings › "Blend Text Colors Using Gamma" drives the compositor's
//! application-wide text gamma (`photocraft_compose::psblend::text_gamma`).
//!
//! This test lives in its own integration-test binary (its own process) on purpose: switching
//! the gamma away from the default changes how every type layer composites, so running it next
//! to the engine's unit tests let a concurrent test flatten one document under two gammas
//! (`canvas_geom::tests::turns_at_every_depth_and_mode` compares the composite before an edit
//! with the one after its undo, and failed on Windows CI that way). Keep any other test that
//! changes the gamma in this file.

use photocraft_engine::Session;
use serde_json::json;

#[test]
fn blend_text_gamma_setting() {
    let mut s = Session::new();
    assert!((s.color.settings.blend_text_gamma - 1.45).abs() < 1e-6);
    assert!(s.execute("edit.colorSettings", json!({"blendTextGamma": 3.0})).is_err());
    // The compositor's (application-wide) gamma follows the setting.
    s.execute("edit.colorSettings", json!({"blendTextGamma": 1.45})).unwrap();
    assert!((photocraft_compose::psblend::text_gamma() - 1.45).abs() < 1e-6);
    let r = s.execute("edit.colorSettings", json!({"blendTextGamma": false})).unwrap();
    assert_eq!(r["settings"]["blendTextGamma"], 1.0);
    assert_eq!(photocraft_compose::psblend::text_gamma(), 1.0);
    s.execute("edit.colorSettings", json!({"blendTextGamma": 1.8})).unwrap();
    assert!((photocraft_compose::psblend::text_gamma() - 1.8).abs() < 1e-6);
    s.execute("edit.colorSettings", json!({"blendTextGamma": true})).unwrap();
    assert!((s.color.settings.blend_text_gamma - 1.45).abs() < 1e-6);
    assert!((photocraft_compose::psblend::text_gamma() - 1.45).abs() < 1e-6);
}
