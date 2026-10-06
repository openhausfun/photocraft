//! Persistent brush preset store: user and imported (`.abr`) brush presets survive a restart.
//!
//! Sampled tips would bloat the preferences document, so brush presets live in their own store,
//! a directory next to the preferences (desktop: `<config dir>/Presets`):
//!
//! - `<group>-<hash>.pcbrushes`: one JSON file per preset group (folder) holding the presets'
//!   settings with every bitmap (sampled tip, Dual Brush tip, texture tile) replaced by a
//!   reference to a tip file;
//! - `tips/<hash>.pctip`: content-addressed bitmaps, deflated, stored at 8 bits per sample when
//!   the tip came from 8-bit data and 16 bits otherwise. Renaming or re-grouping a preset only
//!   rewrites a small JSON file; a tip no group references any more is deleted;
//! - `index.json`: group order, the order of every preset (built-ins included, so drag and drop
//!   in the Brushes panel persists) and the built-in presets the user deleted.
//!
//! Built-in presets are never written (they are regenerated on start). The store syncs after
//! every command that changes the brush presets ([`Session::brush_presets_changed`]); only groups
//! whose content changed are rewritten. Every write is atomic (temp file + rename) and bounded
//! ([`MAX_GROUP_BYTES`], [`MAX_TIP_BYTES`], [`MAX_STORE_BYTES`]); corrupt or oversized files are
//! skipped with a warning and left on disk, never a panic.
//!
//! A [`Session`] has no store by default, so headless CLI/MCP sessions and tests never write.
//! The desktop app opens the store on a background thread ([`open`]) and attaches it with
//! [`Session::attach_preset_store`] once loaded, so the first frame never waits for it. The web
//! app has no store (localStorage is too small for sampled tips): its brush presets are
//! session-only. Gradient presets, including imported `.grd` groups, are small and persist with
//! the preferences document (`presets.gradients`).

use std::collections::{HashMap, HashSet};
use std::io::Read;

use photocraft_paint::{BrushPreset, BrushSettings, GrayTile, Pattern, TipShape};
use serde::{Deserialize, Serialize};

use crate::Session;

/// Group file extension.
pub const GROUP_EXT: &str = "pcbrushes";
/// Tip file extension (inside [`TIPS_DIR`]).
pub const TIP_EXT: &str = "pctip";
/// Subdirectory holding the tip bitmaps.
pub const TIPS_DIR: &str = "tips";
/// Group order and deleted built-ins.
pub const INDEX_FILE: &str = "index.json";
/// Largest group file read or written.
pub const MAX_GROUP_BYTES: u64 = 16 << 20;
/// Largest tip file read or written.
pub const MAX_TIP_BYTES: u64 = 64 << 20;
/// Largest index file read.
pub const MAX_INDEX_BYTES: u64 = 4 << 20;
/// The whole store never grows beyond this; further groups are skipped with a warning.
pub const MAX_STORE_BYTES: u64 = 2 << 30;
/// Largest tip side accepted from disk.
pub const MAX_TIP_SIDE: u32 = 16384;
/// Largest tip sample count accepted from disk (64 Mi samples).
pub const MAX_TIP_SAMPLES: u64 = 1 << 26;
/// Most group files loaded.
pub const MAX_GROUPS: usize = 4096;
/// Most presets loaded from one group file.
pub const MAX_PRESETS_PER_GROUP: usize = 50_000;

const GROUP_FORMAT: &str = "photocraft-brush-group";
const TIP_MAGIC: &[u8; 6] = b"PCTIP1";
const TIP_HEADER: usize = 6 + 1 + 4 + 4;

/// Where the store's files live. File names are relative (`name.pcbrushes`, `tips/x.pctip`).
pub trait PresetBackend: Send {
    /// Every file in the store with its size in bytes (the top level plus [`TIPS_DIR`]).
    /// A missing store is empty, not an error.
    fn list(&self) -> Result<Vec<(String, u64)>, String>;
    /// Read a file, failing when it is larger than `max` bytes.
    fn read(&self, name: &str, max: u64) -> Result<Vec<u8>, String>;
    /// Replace a file atomically (a crash leaves the old or the new file, never half of one).
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// Delete a file; deleting a missing file succeeds.
    fn remove(&self, name: &str) -> Result<(), String>;
}

/// Store file names are generated (`[A-Za-z0-9 _.-]`, optionally under `tips/`); anything else
/// (path separators, `..`) is refused so a backend never leaves its directory.
fn valid_name(name: &str) -> bool {
    let leaf = name.strip_prefix("tips/").unwrap_or(name);
    !leaf.is_empty() && leaf.len() <= 128 && !leaf.starts_with('.') && leaf.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.'))
}

/// An in-memory store (tests, and frontends without a file system). Clones share the files.
#[derive(Clone, Default)]
pub struct MemBackend {
    pub files: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>>,
}

impl MemBackend {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, std::collections::BTreeMap<String, Vec<u8>>>, String> {
        self.files.lock().map_err(|_| "preset store lock poisoned".to_string())
    }
}

impl PresetBackend for MemBackend {
    fn list(&self) -> Result<Vec<(String, u64)>, String> {
        Ok(self.lock()?.iter().map(|(k, v)| (k.clone(), v.len() as u64)).collect())
    }
    fn read(&self, name: &str, max: u64) -> Result<Vec<u8>, String> {
        let files = self.lock()?;
        let b = files.get(name).ok_or_else(|| format!("{name}: not found"))?;
        if b.len() as u64 > max {
            return Err(format!("{name}: too large ({} bytes)", b.len()));
        }
        Ok(b.clone())
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if !valid_name(name) {
            return Err(format!("invalid store file name `{name}`"));
        }
        self.lock()?.insert(name.to_string(), bytes.to_vec());
        Ok(())
    }
    fn remove(&self, name: &str) -> Result<(), String> {
        self.lock()?.remove(name);
        Ok(())
    }
}

/// A store directory on disk (desktop).
#[cfg(not(target_arch = "wasm32"))]
pub struct DirBackend {
    root: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl DirBackend {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        DirBackend { root: root.into() }
    }
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
    fn path(&self, name: &str) -> Result<std::path::PathBuf, String> {
        if !valid_name(name) {
            return Err(format!("invalid store file name `{name}`"));
        }
        Ok(self.root.join(name))
    }
    fn list_dir(&self, sub: &str, out: &mut Vec<(String, u64)>) -> Result<(), String> {
        let dir = if sub.is_empty() { self.root.clone() } else { self.root.join(sub) };
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let Some(leaf) = entry.file_name().to_str().map(str::to_string) else { continue };
            let name = if sub.is_empty() { leaf } else { format!("{sub}/{leaf}") };
            if valid_name(&name) {
                out.push((name, meta.len()));
            }
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl PresetBackend for DirBackend {
    fn list(&self) -> Result<Vec<(String, u64)>, String> {
        let mut out = Vec::new();
        self.list_dir("", &mut out)?;
        self.list_dir(TIPS_DIR, &mut out)?;
        Ok(out)
    }
    fn read(&self, name: &str, max: u64) -> Result<Vec<u8>, String> {
        let path = self.path(name)?;
        let f = std::fs::File::open(&path).map_err(|e| format!("{name}: {e}"))?;
        let mut out = Vec::new();
        f.take(max.saturating_add(1)).read_to_end(&mut out).map_err(|e| format!("{name}: {e}"))?;
        if out.len() as u64 > max {
            return Err(format!("{name}: larger than {max} bytes"));
        }
        Ok(out)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.path(name)?;
        let dir = path.parent().ok_or_else(|| format!("{name}: no parent directory"))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        photocraft_format::atomic_write(&path, bytes).map_err(|e| format!("{name}: {e}"))
    }
    fn remove(&self, name: &str) -> Result<(), String> {
        match std::fs::remove_file(self.path(name)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{name}: {e}")),
        }
    }
}

// ------------------------------------------------------------------ tips

/// Content hash of a tip (32 hex digits); the tip file's name.
fn tip_hash(t: &GrayTile) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&t.width.to_le_bytes());
    h.update(&t.height.to_le_bytes());
    let mut buf = Vec::with_capacity(8192);
    for chunk in t.data.chunks(4096) {
        buf.clear();
        for v in chunk {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        h.update(&buf);
    }
    h.finalize().to_hex()[..32].to_string()
}

/// `PCTIP1`, bits (8|16), width, height (u32 LE), then the zlib-deflated samples.
pub fn encode_tip(t: &GrayTile) -> Vec<u8> {
    use std::io::Write;
    let eight = t.data.iter().all(|v| v % 257 == 0);
    let mut out = Vec::with_capacity(TIP_HEADER + t.data.len() / 4);
    out.extend_from_slice(TIP_MAGIC);
    out.push(if eight { 8 } else { 16 });
    out.extend_from_slice(&t.width.to_le_bytes());
    out.extend_from_slice(&t.height.to_le_bytes());
    let mut z = flate2::write::ZlibEncoder::new(out, flate2::Compression::fast());
    let mut buf = Vec::with_capacity(8192);
    let mut ok = true;
    for chunk in t.data.chunks(4096) {
        buf.clear();
        if eight {
            buf.extend(chunk.iter().map(|v| (v / 257) as u8));
        } else {
            for v in chunk {
                buf.extend_from_slice(&v.to_le_bytes());
            }
        }
        ok &= z.write_all(&buf).is_ok();
    }
    match z.finish() {
        Ok(v) if ok => v,
        _ => Vec::new(),
    }
}

/// Inverse of [`encode_tip`]; any malformed, truncated or oversized input is an error.
pub fn decode_tip(bytes: &[u8]) -> Result<GrayTile, String> {
    let head = bytes.get(..TIP_HEADER).ok_or("truncated tip header")?;
    if &head[..6] != TIP_MAGIC {
        return Err("not a PhotoCraft tip".into());
    }
    let bits = head[6];
    let u32_at = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    let (w, h) = (u32_at(7), u32_at(11));
    if w == 0 || h == 0 || w > MAX_TIP_SIDE || h > MAX_TIP_SIDE {
        return Err(format!("bad tip size {w}×{h}"));
    }
    let n = u64::from(w) * u64::from(h);
    if n > MAX_TIP_SAMPLES {
        return Err(format!("tip too large ({w}×{h})"));
    }
    let bytes_per = match bits {
        8 => 1u64,
        16 => 2,
        b => return Err(format!("unsupported tip depth {b}")),
    };
    let want = n * bytes_per;
    let mut raw = Vec::with_capacity(want as usize);
    flate2::read::ZlibDecoder::new(&bytes[TIP_HEADER..]).take(want + 1).read_to_end(&mut raw).map_err(|e| format!("tip data: {e}"))?;
    if raw.len() as u64 != want {
        return Err(format!("tip data has {} bytes, expected {want}", raw.len()));
    }
    let data: Vec<u16> =
        if bits == 8 { raw.iter().map(|&v| u16::from(v) * 257).collect() } else { raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect() };
    Ok(GrayTile { width: w, height: h, data })
}

// ------------------------------------------------------------------ group files

/// References from a stored preset to its tip files.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TipRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    tip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dual_tip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    texture: Option<String>,
}

impl TipRefs {
    fn hashes(&self) -> impl Iterator<Item = &String> {
        [&self.tip, &self.dual_tip, &self.texture].into_iter().flatten()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPreset {
    name: String,
    brush: BrushSettings,
    #[serde(default)]
    tips: TipRefs,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupFile {
    format: String,
    version: u32,
    group: String,
    presets: Vec<StoredPreset>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct IndexFile {
    version: u32,
    /// Group files in display order.
    groups: Vec<String>,
    /// Built-in presets the user deleted (by name).
    hidden_builtins: Vec<String>,
    /// Every preset's name in library (panel) order; empty when it is the default order.
    order: Vec<String>,
}

/// The group's file name: a readable, sanitised prefix plus a hash of the exact name, so
/// different names never share a file.
pub fn group_file_name(group: &str) -> String {
    let mut stem: String = group.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-') { c } else { '_' }).take(48).collect();
    stem = stem.trim().to_string();
    if stem.is_empty() {
        stem = "Ungrouped".into();
    }
    let h = blake3::hash(group.as_bytes()).to_hex();
    format!("{stem}-{}.{GROUP_EXT}", &h[..8])
}

/// Take the bitmaps out of a preset's settings (they are stored as tip files).
fn split_tips(mut b: BrushSettings) -> (BrushSettings, [Option<GrayTile>; 3]) {
    let tip = match std::mem::take(&mut b.tip) {
        TipShape::Sampled(t) => Some(t),
        TipShape::Round => None,
    };
    let dual = match std::mem::take(&mut b.dual_brush.tip) {
        TipShape::Sampled(t) => Some(t),
        TipShape::Round => None,
    };
    let texture = match std::mem::take(&mut b.texture.pattern) {
        Pattern::Tile(t) => Some(t),
        other => {
            b.texture.pattern = other;
            None
        }
    };
    (b, [tip, dual, texture])
}

/// The opposite of [`split_tips`]: `None` when a referenced tip is missing.
fn join_tips(mut b: BrushSettings, refs: &TipRefs, tips: &HashMap<String, GrayTile>) -> Option<BrushSettings> {
    if let Some(h) = &refs.tip {
        b.tip = TipShape::Sampled(tips.get(h)?.clone());
    }
    if let Some(h) = &refs.dual_tip {
        b.dual_brush.tip = TipShape::Sampled(tips.get(h)?.clone());
    }
    if let Some(h) = &refs.texture {
        b.texture.pattern = Pattern::Tile(tips.get(h)?.clone());
    }
    Some(b)
}

fn tip_file(hash: &str) -> String {
    format!("{TIPS_DIR}/{hash}.{TIP_EXT}")
}

/// What a group looked like when it was last written (or loaded).
struct Synced {
    presets: Vec<BrushPreset>,
    tips: Vec<String>,
    bytes: u64,
}

/// The persistent brush preset store attached to a session (see the module docs).
pub struct PresetStore {
    backend: Box<dyn PresetBackend>,
    /// `tools.presets_rev` at the last sync.
    synced_rev: Option<u64>,
    /// Group file name → its last written content.
    groups: HashMap<String, Synced>,
    /// Tip hash → file size, for every tip file the store owns.
    tips: HashMap<String, u64>,
    index: Option<IndexFile>,
    warnings: Vec<String>,
}

/// A loaded store: the presets on disk plus what the session should hide.
pub struct Opened {
    pub store: PresetStore,
    /// User and imported presets, in group order.
    pub presets: Vec<BrushPreset>,
    /// Built-in presets the user deleted.
    pub hidden_builtins: Vec<String>,
    /// The saved library order (preset names), empty when none was saved.
    pub order: Vec<String>,
    /// Files that were skipped (corrupt, oversized, missing tips).
    pub warnings: Vec<String>,
}

/// Load a store (any thread; the desktop app does this in the background at start).
pub fn open(backend: Box<dyn PresetBackend>) -> Opened {
    let mut warnings = Vec::new();
    let files = backend.list().unwrap_or_else(|e| {
        warnings.push(format!("brush presets: {e}"));
        Vec::new()
    });
    let index: Option<IndexFile> = if files.iter().any(|(n, _)| n == INDEX_FILE) {
        match backend.read(INDEX_FILE, MAX_INDEX_BYTES).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
            Ok(i) => Some(i),
            Err(e) => {
                warnings.push(format!("brush presets: {INDEX_FILE} skipped: {e}"));
                None
            }
        }
    } else {
        None
    };
    // Group files in index order, then the rest by name.
    let mut group_files: Vec<(String, u64)> = files.iter().filter(|(n, _)| !n.contains('/') && n.ends_with(&format!(".{GROUP_EXT}"))).cloned().collect();
    group_files.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(ix) = &index {
        let pos = |n: &str| ix.groups.iter().position(|g| g == n).unwrap_or(usize::MAX);
        group_files.sort_by_key(|(n, _)| pos(n));
    }
    if group_files.len() > MAX_GROUPS {
        warnings.push(format!("brush presets: only the first {MAX_GROUPS} of {} groups were loaded", group_files.len()));
        group_files.truncate(MAX_GROUPS);
    }
    let tip_sizes: HashMap<String, u64> =
        files.iter().filter_map(|(n, sz)| Some((n.strip_prefix(&format!("{TIPS_DIR}/"))?.strip_suffix(&format!(".{TIP_EXT}"))?.to_string(), *sz))).collect();

    // Parse the group files.
    let mut parsed: Vec<(String, u64, GroupFile)> = Vec::new();
    let mut failed = false;
    for (name, size) in group_files {
        let r = backend.read(&name, MAX_GROUP_BYTES).and_then(|b| serde_json::from_slice::<GroupFile>(&b).map_err(|e| e.to_string())).and_then(|g| {
            if g.format != GROUP_FORMAT {
                Err(format!("unknown format `{}`", g.format))
            } else if g.version != 1 {
                Err(format!("unsupported version {}", g.version))
            } else if g.presets.len() > MAX_PRESETS_PER_GROUP {
                Err(format!("{} presets (limit {MAX_PRESETS_PER_GROUP})", g.presets.len()))
            } else {
                Ok(g)
            }
        });
        match r {
            Ok(g) => parsed.push((name, size, g)),
            Err(e) => {
                failed = true;
                warnings.push(format!("brush presets: {name} skipped: {e}"));
            }
        }
    }
    // Decode every referenced tip once.
    let wanted: Vec<String> = {
        let mut seen = HashSet::new();
        parsed.iter().flat_map(|(_, _, g)| g.presets.iter().flat_map(|p| p.tips.hashes())).filter(|h| seen.insert(h.as_str())).cloned().collect()
    };
    let decoded = decode_tips(backend.as_ref(), &wanted, &tip_sizes);
    let mut tips: HashMap<String, GrayTile> = HashMap::with_capacity(decoded.len());
    for (h, r) in wanted.iter().zip(decoded) {
        match r {
            Ok(t) => {
                tips.insert(h.clone(), t);
            }
            Err(e) => warnings.push(format!("brush presets: tip {h} skipped: {e}")),
        }
    }

    let mut store = PresetStore { backend, synced_rev: None, groups: HashMap::new(), tips: HashMap::new(), index: None, warnings: Vec::new() };
    let mut presets = Vec::new();
    for (file, size, g) in parsed {
        let mut items = Vec::with_capacity(g.presets.len());
        let mut hashes = Vec::new();
        let mut missing = 0;
        for sp in g.presets {
            match join_tips(sp.brush, &sp.tips, &tips) {
                Some(brush) => {
                    hashes.extend(sp.tips.hashes().cloned());
                    items.push(BrushPreset { name: sp.name, brush, builtin: false, group: g.group.clone() });
                }
                None => missing += 1,
            }
        }
        if missing > 0 {
            // Left on disk untouched (a sync never deletes a file it did not load).
            failed = true;
            warnings.push(format!("brush presets: {file} skipped: {missing} preset(s) have missing or unreadable tips"));
            continue;
        }
        for h in &hashes {
            if let Some(sz) = tip_sizes.get(h) {
                store.tips.insert(h.clone(), *sz);
            }
        }
        presets.extend(items.iter().cloned());
        store.groups.insert(file, Synced { presets: items, tips: hashes, bytes: size });
    }
    // Tip files nothing references are garbage (e.g. left by a crash), unless a group failed to
    // load: its tips must survive so a fixed file still works.
    if !failed {
        for (h, sz) in &tip_sizes {
            store.tips.entry(h.clone()).or_insert(*sz);
        }
    }
    let hidden_builtins = index.as_ref().map(|i| i.hidden_builtins.clone()).unwrap_or_default();
    let order = index.as_ref().map(|i| i.order.clone()).unwrap_or_default();
    store.index = index;
    Opened { store, presets, hidden_builtins, order, warnings }
}

/// Read and decode tips, in parallel on native targets.
fn decode_tips(backend: &dyn PresetBackend, hashes: &[String], sizes: &HashMap<String, u64>) -> Vec<Result<GrayTile, String>> {
    let read = |h: &String| -> Result<Vec<u8>, String> {
        if !sizes.contains_key(h) {
            return Err("file missing".into());
        }
        backend.read(&tip_file(h), MAX_TIP_BYTES)
    };
    // Reads stay sequential (the backend need not be Sync); decoding is the expensive part.
    let raw: Vec<Result<Vec<u8>, String>> = hashes.iter().map(read).collect();
    let dec = |r: &Result<Vec<u8>, String>| r.as_ref().map_err(Clone::clone).and_then(|b| decode_tip(b));
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        raw.par_iter().map(dec).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        raw.iter().map(dec).collect()
    }
}

/// Open a store directory (desktop). Never fails: problems become warnings.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_dir(root: impl Into<std::path::PathBuf>) -> Opened {
    open(Box::new(DirBackend::new(root)))
}

/// Open a store directory on a background thread; the receiver yields it once loaded.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_dir_async(root: std::path::PathBuf) -> std::sync::mpsc::Receiver<Opened> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("brush-presets".into()).spawn(move || {
        let _ = tx.send(open_dir(root));
    });
    if let Err(e) = spawned {
        // No thread: the caller simply never gets a store (presets stay session-only).
        eprintln!("brush presets: {e}");
    }
    rx
}

impl PresetStore {
    /// Warnings from writes since the last call (the shell shows them in the status bar).
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Total bytes of the files the store owns.
    pub fn bytes(&self) -> u64 {
        self.groups.values().map(|g| g.bytes).sum::<u64>() + self.tips.values().sum::<u64>()
    }

    /// Bring the files in line with `presets` (built-ins skipped): write changed groups, delete
    /// removed ones and their unreferenced tips, update the index.
    pub fn sync(&mut self, presets: &[BrushPreset]) {
        // User presets by group, in first-appearance order.
        let mut order: Vec<(String, Vec<&BrushPreset>)> = Vec::new();
        for p in presets.iter().filter(|p| !p.builtin) {
            match order.iter_mut().find(|(g, _)| *g == p.group) {
                Some((_, v)) => v.push(p),
                None => order.push((p.group.clone(), vec![p])),
            }
        }
        let mut files = Vec::with_capacity(order.len());
        for (group, items) in &order {
            let file = group_file_name(group);
            files.push(file.clone());
            let unchanged = self.groups.get(&file).is_some_and(|s| s.presets.len() == items.len() && s.presets.iter().zip(items).all(|(a, b)| a == *b));
            if unchanged {
                continue;
            }
            if let Err(e) = self.write_group(&file, group, items) {
                self.warnings.push(format!("Couldn't save brush presets `{}`: {e}", if group.is_empty() { "(ungrouped)" } else { group }));
            }
        }
        // Groups that are gone.
        let gone: Vec<String> = self.groups.keys().filter(|f| !files.contains(f)).cloned().collect();
        for f in gone {
            match self.backend.remove(&f) {
                Ok(()) => drop(self.groups.remove(&f)),
                Err(e) => self.warnings.push(format!("Couldn't delete brush presets file: {e}")),
            }
        }
        // Tips nothing references any more.
        let used: HashSet<&String> = self.groups.values().flat_map(|g| g.tips.iter()).collect();
        let unused: Vec<String> = self.tips.keys().filter(|h| !used.contains(h)).cloned().collect();
        for h in unused {
            match self.backend.remove(&tip_file(&h)) {
                Ok(()) => drop(self.tips.remove(&h)),
                Err(e) => self.warnings.push(format!("Couldn't delete brush tip: {e}")),
            }
        }
        // Index: group order and deleted built-ins.
        let hidden: Vec<String> = builtin_names().iter().filter(|n| !presets.iter().any(|p| p.name.eq_ignore_ascii_case(n))).map(|n| n.to_string()).collect();
        let groups: Vec<String> = files.into_iter().filter(|f| self.groups.contains_key(f)).collect();
        // The full order is only saved once it differs from the default (built-ins, then the rest).
        let order: Vec<String> = presets.iter().map(|p| p.name.clone()).collect();
        let default_order: Vec<String> = presets.iter().filter(|p| p.builtin).chain(presets.iter().filter(|p| !p.builtin)).map(|p| p.name.clone()).collect();
        let builtins_in_order = {
            let names = builtin_names();
            let mut it = presets.iter().filter(|p| p.builtin).map(|p| names.iter().position(|n| *n == p.name));
            let first = it.next().flatten();
            it.try_fold(first, |prev, cur| match (prev, cur) {
                (Some(a), Some(b)) if b > a => Some(Some(b)),
                _ => None,
            })
            .is_some()
        };
        let order = if builtins_in_order && order == default_order { Vec::new() } else { order };
        let want = IndexFile { version: 1, groups, hidden_builtins: hidden, order };
        let same = self.index.as_ref().is_some_and(|i| i.groups == want.groups && i.hidden_builtins == want.hidden_builtins && i.order == want.order);
        if !same {
            let empty = want.groups.is_empty() && want.hidden_builtins.is_empty() && want.order.is_empty();
            let r = if empty && self.index.is_none() {
                Ok(())
            } else {
                serde_json::to_vec_pretty(&want).map_err(|e| e.to_string()).and_then(|b| self.backend.write(INDEX_FILE, &b))
            };
            match r {
                Ok(()) => self.index = Some(want),
                Err(e) => self.warnings.push(format!("Couldn't save the brush preset index: {e}")),
            }
        }
    }

    fn write_group(&mut self, file: &str, group: &str, items: &[&BrushPreset]) -> Result<(), String> {
        let others: u64 = self.groups.iter().filter(|(f, _)| f.as_str() != file).map(|(_, g)| g.bytes).sum::<u64>() + self.tips.values().sum::<u64>();
        let mut stored = Vec::with_capacity(items.len());
        let mut hashes = Vec::new();
        let mut new_bytes = 0u64;
        for p in items {
            let (brush, bitmaps) = split_tips(p.brush.clone());
            let mut refs = [None, None, None];
            for (slot, t) in refs.iter_mut().zip(bitmaps) {
                let Some(t) = t else { continue };
                if !t.is_valid() {
                    return Err(format!("preset `{}` has an invalid tip", p.name));
                }
                let h = tip_hash(&t);
                if !self.tips.contains_key(&h) {
                    let bytes = encode_tip(&t);
                    if bytes.is_empty() || bytes.len() as u64 > MAX_TIP_BYTES {
                        return Err(format!("the tip of `{}` is too large to save", p.name));
                    }
                    new_bytes += bytes.len() as u64;
                    if others + new_bytes > MAX_STORE_BYTES {
                        return Err(format!("the preset store is full ({} MB limit)", MAX_STORE_BYTES >> 20));
                    }
                    self.backend.write(&tip_file(&h), &bytes)?;
                    self.tips.insert(h.clone(), bytes.len() as u64);
                }
                hashes.push(h.clone());
                *slot = Some(h);
            }
            let [tip, dual_tip, texture] = refs;
            stored.push(StoredPreset { name: p.name.clone(), brush, tips: TipRefs { tip, dual_tip, texture } });
        }
        let gf = GroupFile { format: GROUP_FORMAT.into(), version: 1, group: group.to_string(), presets: stored };
        let bytes = serde_json::to_vec(&gf).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_GROUP_BYTES {
            return Err(format!("the group is too large to save ({} MB limit)", MAX_GROUP_BYTES >> 20));
        }
        if others + new_bytes + bytes.len() as u64 > MAX_STORE_BYTES {
            return Err(format!("the preset store is full ({} MB limit)", MAX_STORE_BYTES >> 20));
        }
        self.backend.write(file, &bytes)?;
        self.groups.insert(file.to_string(), Synced { presets: items.iter().map(|p| (*p).clone()).collect(), tips: hashes, bytes: bytes.len() as u64 });
        Ok(())
    }
}

fn builtin_names() -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| photocraft_paint::presets::builtin().into_iter().map(|p| p.name).collect())
}

impl Session {
    /// Record a brush preset change (create, delete, rename, import…): the attached store, if
    /// any, syncs after the running command.
    pub fn brush_presets_changed(&mut self) {
        self.tools.presets_rev += 1;
    }

    /// Attach a loaded store: merge its presets into the library (a stored preset replaces the
    /// built-in of the same name; presets created before the store finished loading win), hide
    /// deleted built-ins, then sync. Returns the load warnings.
    pub fn attach_preset_store(&mut self, opened: Opened) -> Vec<String> {
        let Opened { store, presets, hidden_builtins, order, mut warnings } = opened;
        let lib = &mut self.tools.presets;
        lib.retain(|p| !(p.builtin && hidden_builtins.iter().any(|h| h.eq_ignore_ascii_case(&p.name))));
        for p in presets {
            match lib.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&p.name)) {
                Some(x) if x.builtin => *x = p,
                Some(_) => {}
                None => lib.push(p),
            }
        }
        // The saved panel order (stable: presets it doesn't name keep their order, at the end).
        if !order.is_empty() {
            let pos: HashMap<String, usize> = order.iter().enumerate().map(|(i, n)| (n.to_lowercase(), i)).collect();
            lib.sort_by_key(|p| pos.get(&p.name.to_lowercase()).copied().unwrap_or(usize::MAX));
        }
        self.preset_store = Some(store);
        self.brush_presets_changed();
        self.sync_preset_store();
        if let Some(st) = self.preset_store.as_mut() {
            warnings.extend(st.take_warnings());
        }
        warnings
    }

    /// Write pending brush preset changes to the attached store (no-op without one).
    pub fn sync_preset_store(&mut self) {
        let rev = self.tools.presets_rev;
        if let Some(st) = self.preset_store.as_mut()
            && st.synced_rev != Some(rev)
        {
            st.sync(&self.tools.presets);
            st.synced_rev = Some(rev);
        }
    }
}

#[cfg(test)]
mod tests;
