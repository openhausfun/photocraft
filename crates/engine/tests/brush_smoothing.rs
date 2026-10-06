//! Smoothing is a tool option: the Brush starts at Photoshop's 10 %, and picking a brush preset
//! keeps the current smoothing (amount and mode) instead of taking the preset's.

use photocraft_engine::Session;
use serde_json::json;

#[test]
fn presets_keep_the_tool_smoothing() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
    assert_eq!(s.tools.brush.smoothing.amount, 0.1, "Photoshop's default");
    let preset = s.tools.presets.iter().find(|p| p.brush.smoothing.amount == 0.0).map(|p| p.name.clone()).unwrap();
    let smoothed = s.tools.presets.iter().find(|p| p.brush.smoothing.amount > 0.2).map(|p| p.name.clone()).unwrap();
    s.execute("tools.setBrush", json!({"preset": preset})).unwrap();
    assert_eq!(s.tools.brush.smoothing.amount, 0.1);
    s.execute("tools.setBrush", json!({"brush": {"smoothing": {"amount": 0.6, "pulledString": true}}})).unwrap();
    s.execute("tools.setBrush", json!({"preset": smoothed})).unwrap();
    assert_eq!((s.tools.brush.smoothing.amount, s.tools.brush.smoothing.pulled_string), (0.6, true));
    // An explicit smoothing alongside the preset still applies.
    s.execute("tools.setBrush", json!({"preset": preset, "brush": {"smoothing": {"amount": 0.0}}})).unwrap();
    assert_eq!(s.tools.brush.smoothing.amount, 0.0);
}
