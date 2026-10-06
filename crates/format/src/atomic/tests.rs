use super::*;
use std::cell::Cell;

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!("pc-atomic-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Only the target remains: no temporary file was left behind.
fn assert_no_temp(dir: &Path) {
    let leftovers: Vec<_> =
        std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
}

const ORIGINAL: &[u8] = b"the user's precious original document";

/// Real file system with failures injected at chosen steps.
#[derive(Default)]
struct Faulty {
    /// Write this many bytes, then fail like a full disk.
    fail_write_after: Option<usize>,
    /// Fail every rename with this kind.
    fail_rename: Option<io::ErrorKind>,
    /// Fail the first N renames with PermissionDenied (a transient lock), then succeed.
    locked_renames: Cell<u32>,
    renames: Cell<u32>,
}

impl AtomicFs for Faulty {
    fn write_new(&self, tmp: &Path, bytes: &[u8]) -> io::Result<()> {
        if let Some(n) = self.fail_write_after {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(tmp)?;
            f.write_all(&bytes[..n.min(bytes.len())])?;
            return Err(io::Error::new(io::ErrorKind::StorageFull, "injected: no space left on device"));
        }
        RealFs.write_new(tmp, bytes)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.renames.set(self.renames.get() + 1);
        if let Some(kind) = self.fail_rename {
            return Err(io::Error::new(kind, "injected rename failure"));
        }
        if self.locked_renames.get() > 0 {
            self.locked_renames.set(self.locked_renames.get() - 1);
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected: sharing violation"));
        }
        RealFs.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        RealFs.remove(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        RealFs.sync_dir(dir)
    }
    fn copy_permissions(&self, from: &Path, to: &Path) -> io::Result<()> {
        RealFs.copy_permissions(from, to)
    }
}

const NO_RETRY: RenameRetry = RenameRetry { attempts: 0, first_delay_ms: 0 };

#[test]
fn replaces_and_creates() {
    let d = TempDir::new("ok");
    let p = d.0.join("doc.psd");
    atomic_write(&p, b"first").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"first");
    atomic_write(&p, b"second, longer").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"second, longer");
    assert_no_temp(&d.0);
}

#[test]
fn creates_missing_parent_folders() {
    let d = TempDir::new("parent");
    let p = d.0.join("a/b/out.png");
    atomic_write(&p, b"x").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"x");
}

#[test]
fn write_failure_halfway_keeps_original() {
    let d = TempDir::new("write");
    let p = d.0.join("doc.psd");
    std::fs::write(&p, ORIGINAL).unwrap();
    let fs = Faulty { fail_write_after: Some(5), ..Default::default() };
    let e = atomic_write_with(&fs, NO_RETRY, &p, &[7u8; 4096]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::StorageFull);
    assert!(e.to_string().contains("not changed"), "{e}");
    assert_eq!(std::fs::read(&p).unwrap(), ORIGINAL);
    assert_eq!(fs.renames.get(), 0, "never renames after a failed write");
    assert_no_temp(&d.0);
}

#[test]
fn rename_failure_keeps_original() {
    let d = TempDir::new("rename");
    let p = d.0.join("doc.psd");
    std::fs::write(&p, ORIGINAL).unwrap();
    let fs = Faulty { fail_rename: Some(io::ErrorKind::Other), ..Default::default() };
    assert!(atomic_write_with(&fs, NO_RETRY, &p, b"new").is_err());
    assert_eq!(std::fs::read(&p).unwrap(), ORIGINAL);
    assert_no_temp(&d.0);
}

#[test]
fn cross_device_rename_errors_clearly_and_never_writes_in_place() {
    let d = TempDir::new("exdev");
    let p = d.0.join("doc.psd");
    std::fs::write(&p, ORIGINAL).unwrap();
    let fs = Faulty { fail_rename: Some(io::ErrorKind::CrossesDevices), ..Default::default() };
    let e = atomic_write_with(&fs, NO_RETRY, &p, b"new").unwrap_err();
    assert!(e.to_string().contains("different volume"), "{e}");
    assert_eq!(std::fs::read(&p).unwrap(), ORIGINAL);
    assert_no_temp(&d.0);
}

#[test]
fn locked_target_rename_is_retried() {
    let d = TempDir::new("locked");
    let p = d.0.join("doc.psd");
    std::fs::write(&p, ORIGINAL).unwrap();
    let fs = Faulty { locked_renames: Cell::new(2), ..Default::default() };
    atomic_write_with(&fs, RenameRetry { attempts: 3, first_delay_ms: 1 }, &p, b"new").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"new");
    assert_eq!(fs.renames.get(), 3);
    assert_no_temp(&d.0);
}

#[test]
fn lock_that_outlasts_the_retries_keeps_original() {
    let d = TempDir::new("locked2");
    let p = d.0.join("doc.psd");
    std::fs::write(&p, ORIGINAL).unwrap();
    let fs = Faulty { locked_renames: Cell::new(10), ..Default::default() };
    let e = atomic_write_with(&fs, RenameRetry { attempts: 2, first_delay_ms: 1 }, &p, b"new").unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    assert!(e.to_string().contains("locked"), "{e}");
    assert_eq!(fs.renames.get(), 3);
    assert_eq!(std::fs::read(&p).unwrap(), ORIGINAL);
    assert_no_temp(&d.0);
}

#[cfg(unix)]
#[test]
fn read_only_folder_errors_clearly_and_never_writes_in_place() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new("ro");
    let p = d.0.join("doc.psd");
    std::fs::write(&p, ORIGINAL).unwrap();
    // The file itself stays writable: an in-place fallback would succeed and must not happen.
    std::fs::set_permissions(&d.0, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::File::create(d.0.join("probe")).is_ok() {
        return; // running as root: permissions aren't enforced
    }
    let e = atomic_write(&p, b"new").unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    assert!(e.to_string().contains("not writable"), "{e}");
    assert_eq!(std::fs::read(&p).unwrap(), ORIGINAL);
    std::fs::set_permissions(&d.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_no_temp(&d.0);
}

#[cfg(unix)]
#[test]
fn keeps_the_original_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new("mode");
    let p = d.0.join("doc.psd");
    std::fs::write(&p, ORIGINAL).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    atomic_write(&p, b"new").unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn writes_through_a_symlink() {
    let d = TempDir::new("link");
    let real = d.0.join("real.psd");
    let link = d.0.join("link.psd");
    std::fs::write(&real, ORIGINAL).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    atomic_write(&link, b"new").unwrap();
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&real).unwrap(), b"new");
}

#[test]
fn directory_target_or_bare_root_is_an_error_not_a_panic() {
    let d = TempDir::new("dir");
    assert!(atomic_write(&d.0, b"x").is_err());
    assert!(atomic_write(Path::new("/"), b"x").is_err());
    assert!(atomic_write(Path::new(""), b"x").is_err());
}
