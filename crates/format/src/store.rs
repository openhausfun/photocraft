//! Object storage (ZIP or directory), incremental writer and loader.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use photocraft_color::{PixelFormat, SampleType};
use photocraft_doc::Document;
use photocraft_raster::{Rgba8Image, Tile};

use crate::convert::{self, Fetch, Loader, Sink, is_valid_hash, swap_to_le};
use crate::manifest::{FORMAT_VERSION, Hash, Manifest};
use crate::zip::{ZipReader, ZipWriter};
use crate::{FormatError, LoadOptions, Result, SaveOptions};

pub(crate) const MANIFEST: &str = "manifest.json";
pub(crate) const THUMB: &str = "thumb.png";
pub(crate) const COMPOSITE: &str = "composite/preview.png";

fn tile_path(h: &str) -> String {
    format!("tiles/{h}.zst")
}
fn blob_path(h: &str) -> String {
    format!("blobs/{h}.zst")
}

fn compress(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}

fn decompress(data: &[u8], expected: usize, what: &str) -> Result<Vec<u8>> {
    let mut dec = ruzstd::decoding::StreamingDecoder::new(data).map_err(|e| FormatError::corrupt(format!("{what}: zstd: {e}")))?;
    let mut out = Vec::with_capacity(expected.min(64 << 20));
    (&mut dec).take(expected as u64 + 1).read_to_end(&mut out).map_err(|e| FormatError::corrupt(format!("{what}: zstd: {e}")))?;
    if out.len() != expected {
        return Err(FormatError::corrupt(format!("{what}: expected {expected} bytes, got {}", out.len())));
    }
    Ok(out)
}

fn hash_bytes(b: &[u8]) -> Hash {
    blake3::hash(b).to_hex().to_string()
}

fn png(img: &Rgba8Image) -> Result<Vec<u8>> {
    let image = photocraft_codecs::Image::from_u8(img.width, img.height, photocraft_codecs::ChannelLayout::Rgba, img.pixels.clone())
        .map_err(|e| FormatError::Unsupported(format!("preview: {e}")))?;
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| FormatError::Unsupported(format!("preview: {e}")))
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

pub(crate) trait Source {
    fn get(&self, path: &str, max: usize) -> Result<Vec<u8>>;
}

pub(crate) struct ZipSource<'a> {
    zip: ZipReader<'a>,
}

impl<'a> ZipSource<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self> {
        Ok(ZipSource { zip: ZipReader::new(bytes)? })
    }
}

impl Source for ZipSource<'_> {
    fn get(&self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.zip.read_by_name(path, max)
    }
}

pub(crate) struct DirSource {
    pub root: PathBuf,
}

impl Source for DirSource {
    fn get(&self, path: &str, max: usize) -> Result<Vec<u8>> {
        let p = self.root.join(path);
        let len = std::fs::metadata(&p).map_err(|_| FormatError::corrupt(format!("missing `{path}`")))?.len();
        if len > max as u64 {
            return Err(FormatError::LimitExceeded(format!("`{path}` is {len} bytes (max {max})")));
        }
        Ok(std::fs::read(p)?)
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub(crate) fn read_manifest(src: &dyn Source, opts: &LoadOptions) -> Result<Manifest> {
    let raw = src.get(MANIFEST, opts.max_manifest_bytes)?;
    let mut v: serde_json::Value = serde_json::from_slice(&raw)?;
    crate::migrate::migrate(&mut v)?;
    Ok(serde_json::from_value(v)?)
}

struct LoadFetch<'a> {
    src: &'a dyn Source,
    opts: LoadOptions,
    total: u64,
    blobs: HashMap<String, Arc<Vec<u8>>>,
}

impl LoadFetch<'_> {
    fn account(&mut self, n: usize) -> Result<()> {
        self.total += n as u64;
        if self.total > self.opts.max_total_bytes {
            return Err(FormatError::LimitExceeded(format!("bundle expands beyond {} bytes", self.opts.max_total_bytes)));
        }
        Ok(())
    }
}

impl Fetch for LoadFetch<'_> {
    fn tile(&mut self, hash: &str, len: usize) -> Result<Vec<u8>> {
        if !is_valid_hash(hash) {
            return Err(FormatError::corrupt(format!("invalid tile hash `{hash}`")));
        }
        self.account(len)?;
        let path = tile_path(hash);
        let z = self.src.get(&path, len + len / 8 + 4096)?;
        let data = decompress(&z, len, &path)?;
        if hash_bytes(&data) != hash {
            return Err(FormatError::corrupt(format!("{path}: content does not match its hash")));
        }
        Ok(data)
    }

    fn blob(&mut self, hash: &str) -> Result<Arc<Vec<u8>>> {
        if let Some(b) = self.blobs.get(hash) {
            return Ok(b.clone());
        }
        if !is_valid_hash(hash) {
            return Err(FormatError::corrupt(format!("invalid blob hash `{hash}`")));
        }
        let path = blob_path(hash);
        let z = self.src.get(&path, self.opts.max_blob_bytes)?;
        // Blob length is not in the manifest; the frame header bounds it and
        // we cap at max_blob_bytes.
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&z[..]).map_err(|e| FormatError::corrupt(format!("{path}: zstd: {e}")))?;
        let mut data = Vec::new();
        (&mut dec).take(self.opts.max_blob_bytes as u64 + 1).read_to_end(&mut data).map_err(|e| FormatError::corrupt(format!("{path}: zstd: {e}")))?;
        if data.len() > self.opts.max_blob_bytes {
            return Err(FormatError::LimitExceeded(format!("{path} exceeds max_blob_bytes")));
        }
        self.account(data.len())?;
        if hash_bytes(&data) != hash {
            return Err(FormatError::corrupt(format!("{path}: content does not match its hash")));
        }
        let data = Arc::new(data);
        self.blobs.insert(hash.to_owned(), data.clone());
        Ok(data)
    }
}

pub(crate) fn load(src: &dyn Source, opts: &LoadOptions) -> Result<Document> {
    let m = read_manifest(src, opts)?;
    let mut fetch = LoadFetch { src, opts: *opts, total: 0, blobs: HashMap::new() };
    let mut loader = Loader { fetch: &mut fetch, preserve_ids: opts.preserve_ids, max_id: 0, id_map: HashMap::new() };
    let doc = loader.document(&m.document)?;
    if opts.preserve_ids && !convert::reserve_ids_through(loader.max_id) {
        // Ids far beyond our counter: remap instead of risking collisions.
        let fresh = LoadOptions { preserve_ids: false, ..*opts };
        return load(src, &fresh);
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

/// What a save did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveStats {
    /// Distinct tiles referenced by the document.
    pub tiles_total: usize,
    /// Tiles that had to be compressed (ZIP) or written (directory).
    pub tiles_written: usize,
    /// Tiles reused from the previous save.
    pub tiles_reused: usize,
    pub blobs_total: usize,
    pub blobs_written: usize,
    /// Unreferenced objects removed (directory bundles).
    pub objects_removed: usize,
    /// Bytes of the manifest.
    pub manifest_bytes: usize,
}

#[derive(Default)]
struct Collect<'c> {
    tiles: BTreeMap<Hash, (Arc<Tile>, SampleType)>,
    blobs: BTreeMap<Hash, Arc<Vec<u8>>>,
    hash_cache: Option<&'c mut HashMap<usize, (Weak<Tile>, Hash)>>,
}

impl Sink for Collect<'_> {
    fn tile(&mut self, format: PixelFormat, tile: &Arc<Tile>) -> Hash {
        let key = Arc::as_ptr(tile) as usize;
        if let Some(cache) = self.hash_cache.as_deref_mut()
            && let Some((w, h)) = cache.get(&key)
            && w.upgrade().is_some_and(|t| Arc::ptr_eq(&t, tile))
        {
            let h = h.clone();
            self.tiles.entry(h.clone()).or_insert_with(|| (tile.clone(), format.sample));
            return h;
        }
        let h = if cfg!(target_endian = "big") {
            let mut b = tile.bytes().to_vec();
            swap_to_le(&mut b, format.sample);
            hash_bytes(&b)
        } else {
            hash_bytes(tile.bytes())
        };
        if let Some(cache) = self.hash_cache.as_deref_mut() {
            cache.insert(key, (Arc::downgrade(tile), h.clone()));
        }
        self.tiles.entry(h.clone()).or_insert_with(|| (tile.clone(), format.sample));
        h
    }

    fn blob(&mut self, data: &Arc<Vec<u8>>) -> Hash {
        let h = hash_bytes(data);
        self.blobs.entry(h.clone()).or_insert_with(|| data.clone());
        h
    }
}

fn tile_le(t: &Tile, sample: SampleType) -> std::borrow::Cow<'_, [u8]> {
    if cfg!(target_endian = "big") {
        let mut b = t.bytes().to_vec();
        swap_to_le(&mut b, sample);
        std::borrow::Cow::Owned(b)
    } else {
        std::borrow::Cow::Borrowed(t.bytes())
    }
}

/// Incremental `.pcraft` writer. Keep one per open document: it remembers
/// tile hashes (by `Arc` identity) and compressed objects, so re-saving only
/// hashes and compresses tiles that changed.
#[derive(Default)]
pub struct PcraftWriter {
    hash_cache: HashMap<usize, (Weak<Tile>, Hash)>,
    /// Compressed objects by bundle path (ZIP mode).
    compressed: HashMap<String, Arc<Vec<u8>>>,
}

struct Prepared {
    manifest: Vec<u8>,
    objects: BTreeMap<String, Object>,
    previews: Vec<(&'static str, Vec<u8>)>,
    stats: SaveStats,
}

enum Object {
    Tile(Arc<Tile>, SampleType),
    Blob(Arc<Vec<u8>>),
}

impl Object {
    fn compressed(&self) -> Vec<u8> {
        match self {
            Object::Tile(t, s) => compress(&tile_le(t, *s)),
            Object::Blob(b) => compress(b),
        }
    }
}

/// A bundle path and its compressed bytes.
type Compressed<'a> = (&'a String, Arc<Vec<u8>>);

/// Compresses `objects` on scoped worker threads (sequentially on wasm).
fn par_compress<'a>(objects: &[(&'a String, &'a Object)]) -> Result<Vec<Compressed<'a>>> {
    let threads = if cfg!(target_arch = "wasm32") { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 32) };
    if threads < 2 || objects.len() < 2 {
        return Ok(objects.iter().map(|(p, o)| (*p, Arc::new(o.compressed()))).collect());
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads.min(objects.len()))
            .map(|_| {
                sc.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((p, o)) = objects.get(i) else { break };
                        out.push((*p, Arc::new(o.compressed())));
                    }
                    out
                })
            })
            .collect();
        // Join every worker before looking at the results, so one failure can't leave
        // another worker unjoined (the scope would then panic).
        let joined: Vec<_> = hs.into_iter().map(|h| h.join()).collect();
        let mut out = Vec::with_capacity(objects.len());
        for r in joined {
            out.extend(r.map_err(|_| FormatError::Io(std::io::Error::other("compression worker panicked")))?);
        }
        Ok(out)
    })
}

impl PcraftWriter {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare(&mut self, doc: &Document, opts: &SaveOptions) -> Result<Prepared> {
        self.hash_cache.retain(|_, (w, _)| w.strong_count() > 0);
        let mut c = Collect { hash_cache: Some(&mut self.hash_cache), ..Default::default() };
        let document = convert::doc_m(doc, &mut c);
        let mut previews = Vec::new();
        if let Some(t) = &opts.thumbnail {
            previews.push((THUMB, png(t)?));
        }
        if let Some(t) = &opts.composite {
            previews.push((COMPOSITE, png(t)?));
        }
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            generator: format!("photocraft-format {}", env!("CARGO_PKG_VERSION")),
            document,
            thumbnail: opts.thumbnail.as_ref().map(|_| THUMB.to_owned()),
            composite: opts.composite.as_ref().map(|_| COMPOSITE.to_owned()),
        };
        let manifest = serde_json::to_vec_pretty(&manifest)?;
        let stats = SaveStats { tiles_total: c.tiles.len(), blobs_total: c.blobs.len(), manifest_bytes: manifest.len(), ..Default::default() };
        let mut objects = BTreeMap::new();
        for (h, (t, s)) in c.tiles {
            objects.insert(tile_path(&h), Object::Tile(t, s));
        }
        for (h, b) in c.blobs {
            objects.insert(blob_path(&h), Object::Blob(b));
        }
        Ok(Prepared { manifest, objects, previews, stats })
    }

    /// Save as a ZIP bundle in memory.
    pub fn save_zip(&mut self, doc: &Document, opts: &SaveOptions) -> Result<(Vec<u8>, SaveStats)> {
        let p = self.prepare(doc, opts)?;
        let mut stats = p.stats;
        let mut z = ZipWriter::new();
        z.add(MANIFEST, &p.manifest)?;
        for (name, data) in &p.previews {
            z.add(name, data)?;
        }
        let mut next = HashMap::with_capacity(p.objects.len());
        // New objects are compressed on all cores first (zstd dominates a full save).
        let todo: Vec<(&String, &Object)> = p.objects.iter().filter(|(path, _)| !self.compressed.contains_key(*path)).collect();
        let mut fresh: HashMap<&String, Arc<Vec<u8>>> = par_compress(&todo)?.into_iter().collect();
        for (path, obj) in &p.objects {
            let data = match self.compressed.get(path) {
                Some(d) => {
                    if matches!(obj, Object::Tile(..)) {
                        stats.tiles_reused += 1;
                    }
                    d.clone()
                }
                None => {
                    match obj {
                        Object::Tile(..) => stats.tiles_written += 1,
                        Object::Blob(_) => stats.blobs_written += 1,
                    }
                    fresh.remove(path).unwrap_or_else(|| Arc::new(obj.compressed()))
                }
            };
            z.add(path, &data)?;
            next.insert(path.clone(), data);
        }
        self.compressed = next;
        Ok((z.finish()?, stats))
    }

    /// Save into a directory bundle: writes only missing objects, then the
    /// manifest (atomically), then removes unreferenced objects.
    pub fn save_dir(&mut self, doc: &Document, dir: &Path, opts: &SaveOptions) -> Result<SaveStats> {
        let p = self.prepare(doc, opts)?;
        let mut stats = p.stats;
        for sub in ["tiles", "blobs", "composite"] {
            std::fs::create_dir_all(dir.join(sub))?;
        }
        let existing = list_objects(dir)?;
        for (path, obj) in &p.objects {
            if existing.contains(path) {
                if matches!(obj, Object::Tile(..)) {
                    stats.tiles_reused += 1;
                }
                continue;
            }
            match obj {
                Object::Tile(..) => stats.tiles_written += 1,
                Object::Blob(_) => stats.blobs_written += 1,
            }
            write_atomic(&dir.join(path), &obj.compressed())?;
        }
        for (name, data) in &p.previews {
            write_atomic(&dir.join(name), data)?;
        }
        for stale in [THUMB, COMPOSITE] {
            if !p.previews.iter().any(|(n, _)| *n == stale) {
                let _ = std::fs::remove_file(dir.join(stale));
            }
        }
        write_atomic(&dir.join(MANIFEST), &p.manifest)?;
        for path in existing {
            if !p.objects.contains_key(&path) {
                std::fs::remove_file(dir.join(&path))?;
                stats.objects_removed += 1;
            }
        }
        Ok(stats)
    }

    /// Save to `path`: a directory bundle if `path` is an existing directory
    /// or ends with a path separator, otherwise a ZIP file (written atomically).
    pub fn save_path(&mut self, doc: &Document, path: &Path, opts: &SaveOptions) -> Result<SaveStats> {
        let s = path.to_string_lossy();
        if path.is_dir() || s.ends_with('/') || s.ends_with('\\') {
            self.save_dir(doc, path, opts)
        } else {
            let (bytes, stats) = self.save_zip(doc, opts)?;
            write_atomic(path, &bytes)?;
            Ok(stats)
        }
    }
}

fn list_objects(dir: &Path) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for sub in ["tiles", "blobs"] {
        let Ok(rd) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".zst") {
                out.insert(format!("{sub}/{name}"));
            }
        }
    }
    Ok(out)
}

/// Crash-safe replace of one file (see [`crate::atomic`]).
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    crate::atomic::atomic_write(path, data)?;
    Ok(())
}
