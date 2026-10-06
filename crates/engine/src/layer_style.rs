//! `layer.layerStyle.*` commands: add/replace layer effects on a layer.

use photocraft_color::{BlendMode, Color};
use photocraft_doc::{
    Bevel, BevelStyle, BevelTechnique, Contour, Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Gradient, GradientStyle, Satin, Shadow, StrokeFx,
    StrokePosition,
};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, blend_from_str};
use crate::{EngineError, Result, Session};

/// Parses a gradient style name (`linear`, `radial`, `angle`, `reflected`, `diamond`).
pub fn gradient_style(s: &str) -> GradientStyle {
    match s.to_ascii_lowercase().as_str() {
        "radial" => GradientStyle::Radial,
        "angle" => GradientStyle::Angle,
        "reflected" => GradientStyle::Reflected,
        "diamond" => GradientStyle::Diamond,
        _ => GradientStyle::Linear,
    }
}

fn f(p: &Value, k: &str, d: f32) -> f32 {
    p.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}
fn b(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn color(p: &Value, k: &str, d: [f32; 3]) -> Color {
    let c = match p.get(k) {
        Some(Value::Array(a)) if a.len() >= 3 => [0, 1, 2].map(|i| a[i].as_f64().unwrap_or(0.0) as f32),
        Some(Value::String(s)) => {
            let s = s.trim_start_matches('#');
            let h = |i: usize| s.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).map_or(0.0, |v| f32::from(v) / 255.0);
            if s.len() >= 6 { [h(0), h(2), h(4)] } else { d }
        }
        _ => d,
    };
    Color::rgb(c[0], c[1], c[2])
}
fn common(p: &Value, blend: BlendMode, opacity: f32) -> FxCommon {
    FxCommon {
        enabled: b(p, "enabled", true),
        blend: p.get("blend").and_then(Value::as_str).and_then(blend_from_str).unwrap_or(blend),
        opacity: (f(p, "opacity", opacity * 100.0) / 100.0).clamp(0.0, 1.0),
    }
}
fn gradient(p: &Value) -> Gradient {
    Gradient {
        stops: vec![(0.0, color(p, "from", [0.0; 3])), (1.0, color(p, "to", [1.0; 3]))],
        style: gradient_style(p.get("style").and_then(Value::as_str).unwrap_or("linear")),
        angle: f(p, "angle", 90.0),
        scale: f(p, "scale", 100.0) / 100.0,
        reverse: b(p, "reverse", false),
        ..Gradient::default()
    }
}

/// Builds an effect of `kind` from JSON params (Photoshop units: opacity,
/// spread, range in percent; sizes in px; angles in degrees).
pub fn effect_from_params(kind: &str, p: &Value) -> Option<Effect> {
    let shadow = |blend: BlendMode, inner: bool| Shadow {
        common: common(p, blend, 0.75),
        color: color(p, "color", [0.0; 3]),
        angle: f(p, "angle", 120.0),
        use_global_light: b(p, "useGlobalLight", true),
        distance: f(p, "distance", 5.0),
        spread: f(p, if inner { "choke" } else { "spread" }, 0.0) / 100.0,
        size: f(p, "size", 5.0),
        contour: Contour::Linear,
        anti_alias: false,
        noise: f(p, "noise", 0.0) / 100.0,
        knocks_out: !inner && b(p, "knocksOut", true),
    };
    let glow = |inner: bool| Glow {
        common: common(p, BlendMode::Screen, 0.75),
        paint: FxPaint::Color(color(p, "color", [1.0, 1.0, 190.0 / 255.0])),
        technique: if p.get("technique").and_then(Value::as_str) == Some("precise") { GlowTechnique::Precise } else { GlowTechnique::Softer },
        spread: f(p, if inner { "choke" } else { "spread" }, 0.0) / 100.0,
        size: f(p, "size", 5.0),
        contour: Contour::Linear,
        anti_alias: false,
        range: f(p, "range", 50.0) / 100.0,
        jitter: 0.0,
        noise: 0.0,
        source: if p.get("source").and_then(Value::as_str) == Some("center") { GlowSource::Center } else { GlowSource::Edge },
    };
    Some(match kind {
        "dropShadow" => Effect::DropShadow(shadow(BlendMode::Multiply, false)),
        "innerShadow" => Effect::InnerShadow(shadow(BlendMode::Multiply, true)),
        "outerGlow" => Effect::OuterGlow(glow(false)),
        "innerGlow" => Effect::InnerGlow(glow(true)),
        "stroke" => Effect::Stroke(StrokeFx {
            common: common(p, BlendMode::Normal, 1.0),
            size: f(p, "size", 3.0),
            position: match p.get("position").and_then(Value::as_str) {
                Some("inside") => StrokePosition::Inside,
                Some("center") => StrokePosition::Center,
                _ => StrokePosition::Outside,
            },
            paint: if p.get("from").is_some() { FxPaint::Gradient(gradient(p)) } else { FxPaint::Color(color(p, "color", [1.0, 0.0, 0.0])) },
        }),
        "colorOverlay" => Effect::ColorOverlay { common: common(p, BlendMode::Normal, 1.0), color: color(p, "color", [1.0, 0.0, 0.0]) },
        "gradientOverlay" => Effect::GradientOverlay { common: common(p, BlendMode::Normal, 1.0), gradient: gradient(p), dither: false },
        // `name` carries the requested pattern key; `set_effect` resolves it (pattern_cmds).
        "patternOverlay" => Effect::PatternOverlay {
            common: common(p, BlendMode::Normal, 1.0),
            name: p.get("pattern").and_then(Value::as_str).unwrap_or("").to_string(),
            id: String::new(),
            scale: (f(p, "scale", 100.0) / 100.0).clamp(0.01, 10.0),
            angle: f(p, "angle", 0.0),
            link: b(p, "link", true),
            phase: (f(p, "phaseX", 0.0), f(p, "phaseY", 0.0)),
        },
        "satin" => Effect::Satin(Satin {
            common: common(p, BlendMode::Multiply, 0.5),
            color: color(p, "color", [0.0; 3]),
            angle: f(p, "angle", 19.0),
            distance: f(p, "distance", 11.0),
            size: f(p, "size", 14.0),
            contour: Contour::Linear,
            anti_alias: true,
            invert: b(p, "invert", true),
        }),
        "bevelEmboss" => Effect::BevelEmboss(Bevel {
            enabled: b(p, "enabled", true),
            style: match p.get("style").and_then(Value::as_str) {
                Some("outer") => BevelStyle::OuterBevel,
                Some("emboss") => BevelStyle::Emboss,
                Some("pillow") => BevelStyle::PillowEmboss,
                Some("stroke") => BevelStyle::StrokeEmboss,
                _ => BevelStyle::InnerBevel,
            },
            technique: match p.get("technique").and_then(Value::as_str) {
                Some("chiselHard") => BevelTechnique::ChiselHard,
                Some("chiselSoft") => BevelTechnique::ChiselSoft,
                _ => BevelTechnique::Smooth,
            },
            depth: f(p, "depth", 100.0) / 100.0,
            up: p.get("direction").and_then(Value::as_str) != Some("down"),
            size: f(p, "size", 5.0),
            soften: f(p, "soften", 0.0),
            angle: f(p, "angle", 120.0),
            altitude: f(p, "altitude", 30.0),
            use_global_light: b(p, "useGlobalLight", true),
            gloss_contour: Contour::Linear,
            highlight: FxCommon::new(BlendMode::Screen, 0.75),
            highlight_color: Color::WHITE,
            shadow: FxCommon::new(BlendMode::Multiply, 0.75),
            shadow_color: Color::BLACK,
            contour: b(p, "contour", false).then(|| photocraft_doc::BevelContour {
                contour: Contour::Linear,
                range: (f(p, "contourRange", 50.0) / 100.0).clamp(0.01, 1.0),
                anti_alias: false,
            }),
            texture: p.get("texture").and_then(Value::as_str).filter(|t| !t.is_empty()).map(|t| photocraft_doc::BevelTexture {
                name: t.to_string(),
                id: t.to_string(),
                scale: (f(p, "textureScale", 100.0) / 100.0).clamp(0.01, 10.0),
                depth: (f(p, "textureDepth", 100.0) / 100.0).clamp(-10.0, 10.0),
                invert: b(p, "textureInvert", false),
                link: b(p, "textureLink", true),
                phase: (0.0, 0.0),
            }),
        }),
        _ => return None,
    })
}

fn same_kind(a: &Effect, b: &Effect) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn set_effect(s: &mut Session, p: &Value, kind: &str) -> Result<Value> {
    let fx = effect_from_params(kind, p).ok_or_else(|| EngineError::Other(format!("unknown effect {kind}")))?;
    let (fx, pattern) = crate::pattern_cmds::resolve_effect(s, fx)?;
    let label = format!("Layer Style: {}", fx.label());
    let add = b(p, "add", false);
    let id = match p.get("layer").and_then(Value::as_u64) {
        Some(id) => photocraft_doc::LayerId(id),
        None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into()))?,
    };
    s.edit(&label, |doc, _| {
        if let Some(pat) = &pattern {
            crate::pattern_cmds::ensure_in_doc(doc, pat);
        }
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        l.effects.enabled = true;
        match l.effects.items.iter_mut().find(|e| same_kind(e, &fx)) {
            Some(slot) if !add => *slot = fx,
            _ => l.effects.items.push(fx),
        }
        Ok(())
    })?;
    Ok(json!({ "layer": id.0 }))
}

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.filter(|id| d.doc.layer(*id).is_some()).map(|_| ()).ok_or_else(|| "no active layer".into())
}

macro_rules! style_cmd {
    ($kind:literal, $label:literal, $params:literal) => {
        CommandSpec {
            id: concat!("layer.layerStyle.", $kind),
            label: $label,
            menu: &["Layer", "Layer Style"],
            shortcut: None,
            params: $params,
            enabled: has_layer,
            run: |s, p| set_effect(s, p, $kind),
            journal: true,
        }
    };
}

/// The `layer.layerStyle.*` command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        style_cmd!(
            "dropShadow",
            "Drop Shadow…",
            r##"{"color":"#rrggbb","opacity":0..100=75,"blend":str="multiply","angle":deg=120,"useGlobalLight":bool,"distance":px=5,"spread":0..100,"size":px=5,"knocksOut":bool,"add":bool,"layer":id}"##
        ),
        style_cmd!(
            "innerShadow",
            "Inner Shadow…",
            r##"{"color":"#rrggbb","opacity":0..100=75,"blend":str,"angle":deg,"distance":px,"choke":0..100,"size":px,"add":bool}"##
        ),
        style_cmd!(
            "outerGlow",
            "Outer Glow…",
            r##"{"color":"#rrggbb","opacity":0..100=75,"blend":str="screen","technique":"softer|precise","spread":0..100,"size":px,"range":0..100,"add":bool}"##
        ),
        style_cmd!(
            "innerGlow",
            "Inner Glow…",
            r##"{"color":"#rrggbb","opacity":0..100=75,"blend":str="screen","technique":"softer|precise","source":"edge|center","choke":0..100,"size":px,"add":bool}"##
        ),
        style_cmd!(
            "stroke",
            "Stroke…",
            r##"{"size":px=3,"position":"outside|inside|center","color":"#rrggbb","from":"#rrggbb","to":"#rrggbb","style":str,"angle":deg,"opacity":0..100,"blend":str,"add":bool}"##
        ),
        style_cmd!("colorOverlay", "Color Overlay…", r##"{"color":"#rrggbb","opacity":0..100=100,"blend":str,"add":bool}"##),
        style_cmd!(
            "gradientOverlay",
            "Gradient Overlay…",
            r##"{"from":"#rrggbb","to":"#rrggbb","style":"linear|radial|angle|reflected|diamond","angle":deg=90,"scale":10..150=100,"reverse":bool,"opacity":0..100,"blend":str,"add":bool}"##
        ),
        style_cmd!(
            "patternOverlay",
            "Pattern Overlay…",
            r##"{"pattern":id|name?=first library pattern,"opacity":0..100=100,"blend":str,"scale":1..1000=100,"angle":deg=0,"link":bool=true,"phaseX":px,"phaseY":px,"add":bool}"##
        ),
        style_cmd!(
            "bevelEmboss",
            "Bevel & Emboss…",
            r##"{"style":"inner|outer|emboss|pillow|stroke","technique":"smooth|chiselHard|chiselSoft","contour":bool,"contourRange":1..100=50,"texture":pattern id|name?,"textureScale":1..1000=100,"textureDepth":-1000..1000=100,"textureInvert":bool,"textureLink":bool=true,"depth":1..1000=100,"direction":"up|down","size":px=5,"soften":px,"angle":deg,"altitude":deg,"add":bool}"##
        ),
        style_cmd!("satin", "Satin…", r##"{"color":"#rrggbb","opacity":0..100=50,"blend":str,"angle":deg,"distance":px,"size":px,"invert":bool,"add":bool}"##),
        CommandSpec {
            id: "layer.layerStyle.clear",
            label: "Clear Layer Style",
            menu: &["Layer", "Layer Style"],
            shortcut: None,
            params: r##"{"layer":id}"##,
            enabled: has_layer,
            run: |s, p| {
                let id = match p.get("layer").and_then(Value::as_u64) {
                    Some(id) => photocraft_doc::LayerId(id),
                    None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into()))?,
                };
                s.edit("Clear Layer Style", |doc, _| {
                    let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                    l.effects = photocraft_doc::Effects { enabled: true, ..Default::default() };
                    Ok(())
                })?;
                Ok(Value::Null)
            },
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s
    }

    fn effects(s: &Session) -> Vec<Effect> {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().effects.items.clone()
    }

    #[test]
    fn bevel_command_sets_technique_contour_and_texture() {
        let mut s = session();
        s.execute("layer.layerStyle.bevelEmboss", json!({"style": "pillow", "technique": "chiselHard", "contour": true, "contourRange": 70, "texture": "Bubbles", "textureDepth": -200, "textureInvert": true})).unwrap();
        let fx = effects(&s);
        let Effect::BevelEmboss(b) = &fx[0] else { panic!("{fx:?}") };
        assert_eq!((b.style, b.technique), (photocraft_doc::BevelStyle::PillowEmboss, BevelTechnique::ChiselHard));
        assert!((b.contour.as_ref().unwrap().range - 0.7).abs() < 1e-6);
        let t = b.texture.as_ref().unwrap();
        assert_eq!((t.name.as_str(), t.depth, t.invert), ("Bubbles", -2.0, true));
    }

    #[test]
    fn every_style_command_adds_its_effect() {
        for kind in ["dropShadow", "innerShadow", "outerGlow", "innerGlow", "stroke", "colorOverlay", "gradientOverlay", "bevelEmboss", "satin"] {
            let mut s = session();
            s.execute(&format!("layer.layerStyle.{kind}"), json!({})).unwrap();
            let fx = effects(&s);
            assert_eq!(fx.len(), 1, "{kind}");
            assert!(fx[0].enabled());
        }
    }

    #[test]
    fn params_are_applied_and_replace_or_add() {
        let mut s = session();
        s.execute("layer.layerStyle.stroke", json!({"size": 7, "position": "inside", "color": "#00ff00", "opacity": 50})).unwrap();
        match &effects(&s)[0] {
            Effect::Stroke(st) => {
                assert_eq!(st.size, 7.0);
                assert_eq!(st.position, StrokePosition::Inside);
                assert_eq!(st.common.opacity, 0.5);
                assert_eq!(st.paint, FxPaint::Color(Color::rgb(0.0, 1.0, 0.0)));
            }
            other => panic!("{other:?}"),
        }
        s.execute("layer.layerStyle.stroke", json!({"size": 2})).unwrap();
        assert_eq!(effects(&s).len(), 1, "replaces");
        s.execute("layer.layerStyle.stroke", json!({"size": 4, "add": true})).unwrap();
        assert_eq!(effects(&s).len(), 2, "adds a second instance");
        s.execute("layer.layerStyle.dropShadow", json!({"blend": "normal", "distance": 12})).unwrap();
        assert!(matches!(&effects(&s)[2], Effect::DropShadow(sh) if sh.common.blend == BlendMode::Normal && sh.distance == 12.0));
    }

    #[test]
    fn clear_and_undo() {
        let mut s = session();
        s.execute("layer.layerStyle.colorOverlay", json!({"color": "#ff0000"})).unwrap();
        s.execute("layer.layerStyle.clear", json!({})).unwrap();
        assert!(effects(&s).is_empty());
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(effects(&s).len(), 1);
    }

    #[test]
    fn color_overlay_renders() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        s.execute("layer.layerStyle.colorOverlay", json!({"color": "#ff0000"})).unwrap();
        let px: Vec<f32> = serde_json::from_value(s.execute("document.pixel", json!({"x": 3, "y": 3})).unwrap()).unwrap();
        assert!(px[0] > 0.99 && px[1] < 0.01, "{px:?}");
    }

    #[test]
    fn gradient_styles_parse() {
        assert_eq!(gradient_style("Radial"), GradientStyle::Radial);
        assert_eq!(gradient_style("diamond"), GradientStyle::Diamond);
        assert_eq!(gradient_style("?"), GradientStyle::Linear);
    }
}
