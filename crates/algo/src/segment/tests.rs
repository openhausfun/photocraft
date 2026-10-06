use super::gmm::Gmm;
use super::maxflow::Graph;
use super::*;
use photocraft_geom::Rect;

// ---------- max-flow ----------

#[test]
fn maxflow_tiny_graph() {
    // s→a 3, s→b 2, a→b 1, a→t 2, b→t 3: max flow 5.
    let mut g = Graph::with_capacity(2, 1);
    g.add_tweights(0, 3.0, 2.0);
    g.add_tweights(1, 2.0, 3.0);
    g.add_edge(0, 1, 1.0, 0.0);
    assert_eq!(g.maxflow(), 5.0);
}

#[test]
fn maxflow_clrs_network() {
    // The classic textbook network (max flow 23): v1..v4 = 0..3.
    let mut g = Graph::with_capacity(4, 5);
    g.add_tweights(0, 16.0, 0.0);
    g.add_tweights(1, 13.0, 0.0);
    g.add_tweights(2, 0.0, 20.0);
    g.add_tweights(3, 0.0, 4.0);
    g.add_edge(0, 1, 10.0, 4.0);
    g.add_edge(0, 2, 12.0, 0.0);
    g.add_edge(2, 1, 9.0, 0.0);
    g.add_edge(1, 3, 14.0, 0.0);
    g.add_edge(3, 2, 7.0, 0.0);
    assert_eq!(g.maxflow(), 23.0);
    // Min cut {s, v1, v2, v4} | {v3, t}.
    assert!(g.in_source(0) && g.in_source(1) && g.in_source(3));
    assert!(!g.in_source(2));
    assert_eq!(g.residual_out_of_source_side(), 0.0);
}

/// Reference max flow (Edmonds–Karp on a dense matrix; node 0 = s, n−1 = t).
fn edmonds_karp(cap: &mut [Vec<f64>]) -> f64 {
    let n = cap.len();
    let mut flow = 0.0;
    loop {
        let mut prev = vec![usize::MAX; n];
        prev[0] = 0;
        let mut q = std::collections::VecDeque::from([0usize]);
        while let Some(u) = q.pop_front() {
            for v in 0..n {
                if prev[v] == usize::MAX && cap[u][v] > 1e-9 {
                    prev[v] = u;
                    q.push_back(v);
                }
            }
        }
        if prev[n - 1] == usize::MAX {
            return flow;
        }
        let mut b = f64::MAX;
        let mut v = n - 1;
        while v != 0 {
            b = b.min(cap[prev[v]][v]);
            v = prev[v];
        }
        let mut v = n - 1;
        while v != 0 {
            cap[prev[v]][v] -= b;
            cap[v][prev[v]] += b;
            v = prev[v];
        }
        flow += b;
    }
}

#[test]
fn maxflow_matches_reference_on_random_graphs() {
    let mut rng = Rng::new(7);
    for trial in 0..60 {
        let n = 3 + (trial % 12);
        let mut g = Graph::with_capacity(n, n * 3);
        let mut cap = vec![vec![0.0f64; n + 2]; n + 2];
        let mut edges = Vec::new();
        for i in 0..n {
            let (s, t) = ((rng.f32() * 10.0).round(), (rng.f32() * 10.0).round());
            let (s, t) = (if rng.f32() < 0.4 { 0.0 } else { s }, if rng.f32() < 0.4 { 0.0 } else { t });
            g.add_tweights(i, s, t);
            cap[0][i + 1] += s as f64;
            cap[i + 1][n + 1] += t as f64;
        }
        for i in 0..n {
            for j in i + 1..n {
                if rng.f32() < 0.35 {
                    let (a, b) = ((rng.f32() * 8.0).round(), (rng.f32() * 8.0).round());
                    g.add_edge(i, j, a, b);
                    cap[i + 1][j + 1] += a as f64;
                    cap[j + 1][i + 1] += b as f64;
                    edges.push((i, j, a, b));
                }
            }
        }
        let orig = cap.clone();
        let reference = edmonds_karp(&mut cap);
        let f = g.maxflow();
        assert!((f - reference).abs() < 1e-3, "trial {trial}: {f} vs {reference}");
        // The reported cut has exactly that capacity.
        let side = |i: usize| g.in_source(i);
        let mut cut = 0.0;
        for i in 0..n {
            if side(i) {
                cut += orig[i + 1][n + 1];
            } else {
                cut += orig[0][i + 1];
            }
        }
        for &(i, j, a, b) in &edges {
            if side(i) && !side(j) {
                cut += a as f64;
            }
            if side(j) && !side(i) {
                cut += b as f64;
            }
        }
        assert!((cut - reference).abs() < 1e-3, "trial {trial}: cut {cut} vs flow {reference}");
    }
}

#[test]
fn grid_cut_separates_two_halves() {
    let img = RgbImage::from_fn(20, 10, |x, _| if x < 10 { [0.9, 0.1, 0.1] } else { [0.1, 0.1, 0.9] });
    let n = img.w * img.h;
    let mut fixed = vec![FREE; n];
    fixed[5 * 20 + 1] = HARD_FG;
    fixed[5 * 20 + 18] = HARD_BG;
    let zeros = vec![0.0; n];
    let beta = contrast_beta(&img);
    let cut = grid_cut(&img, &zeros, &zeros, &fixed, 50.0, beta);
    for y in 0..10 {
        for x in 0..20 {
            assert_eq!(cut[y * 20 + x], x < 10, "({x},{y})");
        }
    }
}

// ---------- GMM ----------

#[test]
fn gmm_recovers_synthetic_clusters() {
    let mut rng = Rng::new(3);
    let truth = [([0.2f32, 0.3, 0.8], 0.03f32, 0.5f32), ([0.8, 0.2, 0.1], 0.05, 0.3), ([0.5, 0.9, 0.4], 0.02, 0.2)];
    let mut samples = Vec::new();
    for (mean, sd, w) in truth {
        for _ in 0..(w * 6000.0) as usize {
            samples.push(mean.map(|m| m + sd * rng.normal()));
        }
    }
    let g = Gmm::fit(&samples, 3, gmm::DEFAULT_REG).unwrap();
    assert_eq!(g.comps.len(), 3);
    for (mean, sd, w) in truth {
        let c = g.comps.iter().min_by(|a, b| d2(a.mean, mean).total_cmp(&d2(b.mean, mean))).unwrap();
        assert!(d2(c.mean, mean).sqrt() < 0.01, "{:?} vs {mean:?}", c.mean);
        assert!((c.weight - w).abs() < 0.03, "{} vs {w}", c.weight);
        let var = c.cov[0][0];
        assert!((var.sqrt() - sd).abs() < 0.01, "sd {} vs {sd}", var.sqrt());
    }
    assert!(g.neg_log([0.2, 0.3, 0.8]) < g.neg_log([0.5, 0.5, 0.5]));
    // Refit keeps the model stable.
    let r = g.refit(&samples).unwrap();
    assert_eq!(r.comps.len(), 3);
}

// ---------- SLIC ----------

#[test]
fn slic_counts_compactness_and_edges() {
    let mut rng = Rng::new(11);
    let (w, h) = (120usize, 120usize);
    let img = RgbImage::from_fn(w, h, |x, _| if x < 60 { [0.8, 0.3, 0.2] } else { [0.2, 0.5, 0.8] });
    let img = RgbImage { px: img.px.iter().map(|p| p.map(|v| (v + 0.02 * rng.normal()).clamp(0.0, 1.0))).collect(), ..img };
    let sp = slic::slic(&img, 64, 10.0, 10);
    assert!((40..=90).contains(&sp.count), "count {}", sp.count);
    let mut bbox = vec![(usize::MAX, usize::MAX, 0usize, 0usize); sp.count];
    let mut left = vec![0usize; sp.count];
    let mut total = vec![0usize; sp.count];
    for y in 0..h {
        for x in 0..w {
            let l = sp.labels[y * w + x] as usize;
            let b = &mut bbox[l];
            *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            total[l] += 1;
            if x < 60 {
                left[l] += 1;
            }
        }
    }
    let s = sp.step;
    for (l, b) in bbox.iter().enumerate() {
        assert!(total[l] > 0);
        assert!(((b.2 - b.0 + 1) as f32) <= 3.0 * s && ((b.3 - b.1 + 1) as f32) <= 3.0 * s, "superpixel {l} too elongated: {b:?} (S={s})");
        // Superpixels respect the strong colour edge.
        let purity = left[l].max(total[l] - left[l]) as f32 / total[l] as f32;
        assert!(purity > 0.95, "superpixel {l} straddles the edge ({purity})");
    }
}

// ---------- GrabCut / object selection ----------

/// A textured disc on a noisy background: (image, ground-truth mask).
pub(crate) fn disc_scene(w: usize, h: usize, cx: f32, cy: f32, r: f32, seed: u64) -> (RgbImage, Vec<bool>) {
    let mut rng = Rng::new(seed);
    let mut img = RgbImage::new(w, h);
    let mut truth = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let inside = dx * dx + dy * dy <= r * r;
            truth[y * w + x] = inside;
            let n = 0.05 * rng.normal();
            img.px[y * w + x] = if inside {
                // Stripes between two warm colours plus noise.
                let t = (((x + y) as f32 * 0.35).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
                [0.85 + 0.05 * t + n, 0.2 + 0.3 * t + n, 0.15 + n]
            } else {
                [0.25 + n, 0.45 + n, 0.7 + n]
            }
            .map(|v| v.clamp(0.0, 1.0));
        }
    }
    (img, truth)
}

pub(crate) fn iou(region: &Region, truth: &[bool], w: usize, h: usize) -> f32 {
    let (mut inter, mut uni) = (0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            let a = region.at(x as i32, y as i32) >= 0.5;
            let b = truth[y * w + x];
            inter += (a && b) as usize;
            uni += (a || b) as usize;
        }
    }
    inter as f32 / uni.max(1) as f32
}

#[test]
fn grabcut_trimap_on_disc() {
    let (img, truth) = disc_scene(120, 100, 60.0, 50.0, 30.0, 1);
    let mut tri = vec![grabcut::BG; img.w * img.h];
    for y in 12..88 {
        for x in 22..98 {
            tri[y * img.w + x] = grabcut::PR_FG;
        }
    }
    grabcut::grabcut(&img, &mut tri, 8).unwrap();
    let (mut inter, mut uni) = (0, 0);
    for (t, g) in tri.iter().zip(&truth) {
        let a = *t == grabcut::PR_FG || *t == grabcut::FG;
        inter += (a && *g) as usize;
        uni += (a || *g) as usize;
    }
    let iou = inter as f32 / uni as f32;
    assert!(iou > 0.95, "IoU {iou}");
}

#[test]
fn object_select_disc_full_resolution() {
    let (img, truth) = disc_scene(200, 160, 100.0, 80.0, 50.0, 2);
    let s = ImageSampler { img: &img, origin: (0, 0) };
    let canvas = Rect::new(0, 0, 200, 160);
    let reg = grabcut::object_select(&s, canvas, Rect::new(40, 20, 160, 140), 1_000_000).unwrap();
    let v = iou(&reg, &truth, 200, 160);
    assert!(v > 0.95, "IoU {v}");
}

#[test]
fn object_select_downsampled_with_band_refinement() {
    let (img, truth) = disc_scene(640, 480, 330.0, 250.0, 150.0, 3);
    let s = ImageSampler { img: &img, origin: (0, 0) };
    let canvas = Rect::new(0, 0, 640, 480);
    // A small working budget forces step > 1 and the full-resolution band re-cut.
    let reg = grabcut::object_select(&s, canvas, Rect::new(160, 80, 500, 420), 20_000).unwrap();
    let v = iou(&reg, &truth, 640, 480);
    assert!(v > 0.95, "IoU {v}");
    // Boundary accuracy: few pixels wrong far (> 2 px) from the true edge.
    let mut far_wrong = 0;
    for y in 0..480 {
        for x in 0..640 {
            let (dx, dy) = (x as f32 + 0.5 - 330.0, y as f32 + 0.5 - 250.0);
            let d = (dx * dx + dy * dy).sqrt() - 150.0;
            if d.abs() > 2.0 && (reg.at(x, y) >= 0.5) != truth[y as usize * 640 + x as usize] {
                far_wrong += 1;
            }
        }
    }
    assert!(far_wrong < 300, "{far_wrong} pixels wrong away from the edge");
}

// ---------- Quick selection ----------

/// Left: textured warm region; right: cool region; strong vertical edge at x = 60.
fn two_regions(w: usize, h: usize) -> RgbImage {
    let mut rng = Rng::new(5);
    RgbImage::from_fn(w, h, |x, y| {
        let n = 0.04 * rng.normal();
        let t = ((x as f32 * 0.5).sin() * (y as f32 * 0.3).cos()) * 0.05;
        if x < 60 { [0.8 + t + n, 0.5 + n, 0.3 + t] } else { [0.2 + n, 0.35 + t, 0.6 + n] }.map(|v| v.clamp(0.0, 1.0))
    })
}

#[test]
fn quick_select_grows_without_leaking() {
    let img = two_regions(140, 100);
    let s = ImageSampler { img: &img, origin: (0, 0) };
    let canvas = Rect::new(0, 0, 140, 100);
    let reg = quick::quick_select(&s, canvas, &[(20.0, 50.0), (35.0, 50.0)], 8.0, quick::WORK_PX).unwrap();
    let mut inside = 0;
    for y in 0..100 {
        for x in 0..140 {
            let v = reg.at(x, y);
            if x >= 62 {
                assert!(v < 0.5, "leaked across the edge at ({x},{y}): {v}");
            }
            if x < 58 && v >= 0.5 {
                inside += 1;
            }
        }
    }
    // Grew over (nearly) the whole left region, far beyond the stroke.
    assert!(inside as f32 > 0.97 * (58.0 * 100.0), "only {inside} pixels selected");
}

#[test]
fn quick_select_stops_at_thin_line() {
    // Same colour on both sides of a dark 2-pixel line.
    let mut rng = Rng::new(9);
    let img = RgbImage::from_fn(120, 80, |x, _| {
        let n = 0.03 * rng.normal();
        if x == 60 || x == 61 { [0.05, 0.05, 0.05] } else { [0.6 + n, 0.6 + n, 0.55 + n] }
    });
    let s = ImageSampler { img: &img, origin: (0, 0) };
    let reg = quick::quick_select(&s, Rect::new(0, 0, 120, 80), &[(30.0, 40.0)], 6.0, quick::WORK_PX).unwrap();
    assert!(reg.at(10, 10) >= 0.5 && reg.at(50, 70) >= 0.5);
    for y in 0..80 {
        for x in 63..120 {
            assert!(reg.at(x, y) < 0.5, "crossed the line at ({x},{y})");
        }
    }
}

#[test]
fn quick_select_downsampled_window() {
    // A larger image with a small working budget: step > 1 plus band refinement.
    let (img, truth) = disc_scene(600, 500, 300.0, 250.0, 180.0, 4);
    let s = ImageSampler { img: &img, origin: (0, 0) };
    let reg = quick::quick_select(&s, Rect::new(0, 0, 600, 500), &[(260.0, 250.0), (340.0, 250.0)], 30.0, 15_000).unwrap();
    let v = iou(&reg, &truth, 600, 500);
    assert!(v > 0.93, "IoU {v}");
}

// ---------- Select subject ----------

#[test]
fn select_subject_finds_centred_object() {
    let (img, truth) = disc_scene(200, 150, 100.0, 75.0, 40.0, 6);
    let s = ImageSampler { img: &img, origin: (0, 0) };
    let reg = subject::select_subject(&s, Rect::new(0, 0, 200, 150)).unwrap();
    let v = iou(&reg, &truth, 200, 150);
    assert!(v > 0.9, "IoU {v}");
}

#[test]
fn saliency_highlights_the_object() {
    let (img, truth) = disc_scene(120, 90, 60.0, 45.0, 22.0, 8);
    let sal = subject::saliency(&img);
    let (mut fi, mut fn_, mut bi, mut bn) = (0.0, 0.0, 0.0, 0.0);
    for (s, t) in sal.iter().zip(&truth) {
        if *t {
            fi += s;
            fn_ += 1.0;
        } else {
            bi += s;
            bn += 1.0;
        }
    }
    assert!(fi / fn_ > 0.6 && bi / bn < 0.25, "fg {} bg {}", fi / fn_, bi / bn);
}

// ---------- Focus area ----------

#[test]
fn focus_area_selects_sharp_half() {
    let mut rng = Rng::new(12);
    let (w, h) = (160usize, 100usize);
    let noise: Vec<f32> = (0..w * h).map(|_| rng.f32()).collect();
    // Right half: heavily blurred copy of the texture.
    let blurred = crate::matting::box_mean(&noise, w, h, 4);
    let img = RgbImage::from_fn(w, h, |x, y| {
        let v = if x < 80 { noise[y * w + x] } else { blurred[y * w + x] };
        [v, v * 0.8, 0.3]
    });
    let s = ImageSampler { img: &img, origin: (0, 0) };
    let reg = focus::focus_area(&s, Rect::new(0, 0, w as i32, h as i32), 0.5, 0.0).unwrap();
    let (mut l, mut r) = (0usize, 0usize);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if reg.at(x, y) >= 0.5 {
                if x < 80 { l += 1 } else { r += 1 }
            }
        }
    }
    assert!(l as f32 > 0.85 * 80.0 * h as f32, "sharp half {l}");
    assert!((r as f32) < 0.1 * 80.0 * h as f32, "blurred half {r}");
}

// ---------- helpers ----------

#[test]
fn sampler_downsampling_matches_image_downsample() {
    let img = RgbImage::from_fn(37, 29, |x, y| [x as f32 / 37.0, y as f32 / 29.0, ((x * y) % 7) as f32 / 7.0]);
    let s = ImageSampler { img: &img, origin: (5, 3) };
    let a = s.rgb_scaled(Rect::new(5, 3, 42, 32), 4);
    let b = img.downsample(4);
    assert_eq!((a.w, a.h), (b.w, b.h));
    for (p, q) in a.px.iter().zip(&b.px) {
        for c in 0..3 {
            assert!((p[c] - q[c]).abs() < 1e-5);
        }
    }
}

#[test]
fn clean_mask_drops_islands_and_fills_holes() {
    let (w, h) = (40, 40);
    let mut m = vec![false; w * h];
    for y in 5..30 {
        for x in 5..30 {
            m[y * w + x] = true;
        }
    }
    m[15 * w + 15] = false; // hole
    m[36 * w + 36] = true; // island
    let c = clean_mask(&m, w, h, 0.1, 0.05);
    assert!(c[15 * w + 15]);
    assert!(!c[36 * w + 36]);
}

#[test]
fn geodesic_distance_jumps_across_edges() {
    let img = two_regions(140, 100);
    let seeds = quick::stroke_mask(&[(20.0, 50.0)], 4.0, 140, 100);
    let d = quick::geodesic(&img, &seeds);
    assert!(d[50 * 140 + 40] < 0.02, "same region {}", d[50 * 140 + 40]);
    assert!(d[50 * 140 + 100] > 0.1, "across the edge {}", d[50 * 140 + 100]);
}

#[test]
fn geodesic_barrier_blocks_oblique_crossings() {
    // A large disc whose slightly tilted edge crosses the image: paths running along the soft
    // edge must not sneak across it.
    let (img, truth) = disc_scene(352, 372, -470.0, 186.0, 668.0, 4);
    let seeds = quick::stroke_mask(&[(175.0, 176.0), (175.0, 196.0)], 5.0, 352, 372);
    assert!(seeds.iter().zip(&truth).all(|(s, t)| !*s || *t));
    let d = quick::geodesic(&img, &seeds);
    assert!(d[186 * 352 + 100] < 0.02);
    for y in (0..372).step_by(31) {
        assert!(d[y * 352 + 291] > 0.1, "row {y}: {}", d[y * 352 + 291]);
    }
}
