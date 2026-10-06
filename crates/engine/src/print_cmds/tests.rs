use super::*;
use photocraft_doc::{Knot, NamedPath, Path, Subpath};

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("photocraft-print-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().into_owned()
}

fn session(mode: &str, depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 300, "height": 150, "mode": mode, "depth": depth, "name": "Print Me.psd", "background": "#3366cc"})).unwrap();
    s
}

fn find(h: &[u8], n: &[u8], from: usize) -> usize {
    from + h[from..].windows(n.len()).position(|w| w == n).unwrap()
}

/// The (inflated) image stream of a print PDF: (width, height, colour space, samples).
fn pdf_image(pdf: &[u8]) -> (u32, u32, String, Vec<u8>) {
    let i = find(pdf, b"/Subtype /Image", 0);
    let end = find(pdf, b">>\nstream\n", i);
    let dict = String::from_utf8_lossy(&pdf[i..end]).into_owned();
    let num = |k: &str| dict.split(k).nth(1).unwrap().split_whitespace().next().unwrap().parse::<u32>().unwrap();
    let cs = dict.split("/ColorSpace ").nth(1).unwrap().split(" /BitsPerComponent").next().unwrap().to_string();
    let len = num("/Length ") as usize;
    let start = end + 10;
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&pdf[start..start + len]), &mut out).unwrap();
    (num("/Width "), num("/Height "), cs, out)
}

#[test]
fn print_dry_run_renders_a_pdf_and_reports_lp() {
    let dir = tmp("print");
    for depth in [8, 16, 32] {
        let mut s = session("rgb", depth);
        let r = s
            .execute(
                "file.print",
                json!({"dryRun": true, "printer": "Office", "copies": 2, "paper": "a4", "cornerCropMarks": true, "registrationMarks": true, "labels": true}),
            )
            .unwrap();
        let cmd: Vec<String> = serde_json::from_value(r["command"].clone()).unwrap();
        assert_eq!(&cmd[..5], &["lp", "-d", "Office", "-n", "2"]);
        assert_eq!(r["sent"], false);
        let pdf = std::fs::read(r["pdf"].as_str().unwrap()).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(String::from_utf8_lossy(&pdf).contains("/MediaBox [0 0 595.28 841.89]"));
        let (w, h, cs, data) = pdf_image(&pdf);
        assert_eq!((w, h), (300, 150));
        assert_eq!(cs, "/DeviceRGB");
        assert_eq!(&data[..3], &[0x33, 0x66, 0xcc]);
        // 300 px at 72 ppi = 300 pt, centred.
        let rect: Vec<f64> = serde_json::from_value(r["imageRect"].clone()).unwrap();
        assert!((rect[2] - 300.0).abs() < 1e-6 && (rect[0] - (595.28 - 300.0) / 2.0).abs() < 1e-6);
        let _ = std::fs::remove_file(r["pdf"].as_str().unwrap());
    }
    // Print to PDF: no spooling by default.
    let mut s = session("gray", 8);
    let out = format!("{dir}/out.pdf");
    let r = s.execute("file.print", json!({"output": out, "scaleToFit": true, "orientation": "landscape"})).unwrap();
    assert_eq!(r["sent"], false);
    assert!(r["command"].is_null());
    let (_, _, cs, _) = pdf_image(&std::fs::read(&out).unwrap());
    assert_eq!(cs, "/DeviceGray");
    let scale = r["scale"].as_f64().unwrap();
    assert!(scale > 200.0, "fit to a landscape letter page: {scale}");
    assert!(s.execute("file.print", json!({"paper": "napkin", "dryRun": true})).is_err());
    assert!(s.execute("file.print", json!({"colorHandling": "photocraftManages", "dryRun": true})).is_err(), "needs a printer profile");
}

#[test]
fn photocraft_manages_colors_converts_to_the_printer_profile() {
    let mut s = session("rgb", 8);
    let r = s
        .execute(
            "file.print",
            json!({"dryRun": true, "colorHandling": "photocraftManages", "printerProfile": "coated-cmyk", "intent": "perceptual", "bpc": false}),
        )
        .unwrap();
    assert_eq!(r["color"]["intent"], "perceptual");
    let pdf = std::fs::read(r["pdf"].as_str().unwrap()).unwrap();
    let (_, _, cs, data) = pdf_image(&pdf);
    assert_eq!(cs, "[/ICCBased 7 0 R]");
    assert!(String::from_utf8_lossy(&pdf).contains("/N 4 /Alternate /DeviceCMYK"));
    assert_eq!(data.len(), 300 * 150 * 4);
    // Blue: lots of cyan, little yellow.
    assert!(data[0] > 150 && data[2] < 60, "{:?}", &data[..4]);
    // The document itself is untouched.
    assert_eq!(s.active().unwrap().doc.mode, ColorMode::Rgb);
    let _ = std::fs::remove_file(r["pdf"].as_str().unwrap());
}

#[test]
fn print_one_copy_repeats_the_last_settings() {
    let mut s = session("rgb", 8);
    s.execute("file.print", json!({"dryRun": true, "copies": 3, "paper": "a5", "scale": 50})).unwrap();
    let r = s.execute("file.printOneCopy", json!({"dryRun": true})).unwrap();
    assert_eq!(r["copies"], 1);
    assert_eq!(r["paper"], json!([419.53, 595.28]));
    assert_eq!(r["scale"], 50.0);
    let cmd: Vec<String> = serde_json::from_value(r["command"].clone()).unwrap();
    assert!(!cmd.contains(&"-n".to_string()));
    let _ = std::fs::remove_file(r["pdf"].as_str().unwrap());
}

#[test]
fn package_copies_links_and_relinks() {
    let dir = tmp("package");
    let link = format!("{dir}/art.png");
    let mut src = Session::new();
    src.execute("file.new", json!({"width": 20, "height": 20, "background": "#ff0000"})).unwrap();
    crate::file_cmds::save_doc(&src.active().unwrap().doc, &link, None).unwrap();
    let mut s = session("rgb", 8);
    s.execute("file.placeLinked", json!({"path": link})).unwrap();
    let r = s.execute("file.package", json!({"dir": format!("{dir}/pkg")})).unwrap();
    let copied = format!("{dir}/pkg/Print Me/Links/art.png");
    assert_eq!(r["links"], json!([copied]));
    assert!(std::path::Path::new(&copied).is_file());
    let docp = r["document"].as_str().unwrap();
    assert!(docp.ends_with("Print Me/Print Me.pcraft"));
    let back = photocraft_io::import("Print Me.pcraft", &std::fs::read(docp).unwrap()).unwrap().document;
    let linked: Vec<String> = back
        .walk()
        .iter()
        .filter_map(|(_, _, l)| match &l.content {
            LayerContent::Smart(so) => match &so.source {
                SmartSource::Linked { path } => Some(path.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(linked.len(), 1);
    assert!(linked[0].ends_with("Links/art.png"), "{linked:?}");
    // The open document still points at the original.
    let open = &s.active().unwrap().doc;
    assert!(
        open.walk()
            .iter()
            .any(|(_, _, l)| matches!(&l.content, LayerContent::Smart(so) if matches!(&so.source, SmartSource::Linked { path } if *path == link)))
    );
}

#[test]
fn paths_export_as_illustrator_postscript() {
    let mut s = session("rgb", 8);
    assert!(!s.is_enabled("file.export.pathsToIllustrator"));
    s.edit("paths", |d, _| {
        let tri = Subpath::polygon(&[(10.0, 10.0), (100.0, 10.0), (50.0, 90.0)]);
        let mut curve = Subpath::polyline(&[(0.0, 0.0), (300.0, 150.0)]);
        curve.knots[0] = Knot::smooth(photocraft_geom::Point::new(0.0, 0.0), photocraft_geom::Point::new(0.0, 0.0), photocraft_geom::Point::new(50.0, 0.0));
        d.paths.push(NamedPath { name: "Shapes".into(), path: Path::new(vec![tri, curve]), psd_raw: None });
        Ok(())
    })
    .unwrap();
    let r = s.execute("file.export.pathsToIllustrator", json!({})).unwrap();
    let ai = r["ai"].as_str().unwrap();
    assert!(ai.starts_with("%!PS-Adobe-2.0 EPSF-1.2"));
    assert!(ai.contains("%%BoundingBox: 0 0 300 150"));
    assert!(ai.contains("*u\n10.0000 140.0000 m\n100.0000 140.0000 L\n"), "{ai}");
    assert!(ai.contains("\nn\n") && ai.contains("\nN\n*U\n"));
    assert!(ai.contains(" C\n"), "curve segment");
    let dir = tmp("ai");
    let r = s.execute("file.export.pathsToIllustrator", json!({"path": format!("{dir}/p.ai"), "paths": "Shapes"})).unwrap();
    assert_eq!(r["paths"], 1);
    assert!(s.execute("file.export.pathsToIllustrator", json!({"paths": "Nope"})).is_err());
}
