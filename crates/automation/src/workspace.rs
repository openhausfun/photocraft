//! Capability-based filesystem policy for untrusted automation paths.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use photocraft_format::atomic::RenameRetry;
use serde_json::Value;

use crate::AutomationError;

const DENIED: &str = "automation filesystem access is not granted";

#[derive(Clone)]
struct RootCapability {
    dir: Arc<Dir>,
}

/// Separate directory capabilities for automation reads and writes.
///
/// Root paths are trusted launch-time configuration. Request paths are always
/// untrusted, forward-slash relative paths resolved by `cap-std` beneath the
/// held directory handle. Absolute paths, parent traversal, alternate
/// separators, Windows prefixes and empty components are rejected before I/O.
#[derive(Clone, Default)]
pub struct AuthorizedWorkspace {
    read: Option<RootCapability>,
    write: Option<RootCapability>,
}

impl AuthorizedWorkspace {
    /// Open the configured roots as capabilities. Either authority may be
    /// omitted; omitted authority fails closed.
    pub fn new(read_root: Option<&Path>, write_root: Option<&Path>) -> Result<Self, AutomationError> {
        Ok(Self { read: open_root(read_root, "read")?, write: open_root(write_root, "write")? })
    }

    /// Read one regular file below the configured read root.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, AutomationError> {
        let relative = relative_path(path)?;
        let root = self.read.as_ref().ok_or_else(|| AutomationError::BadRequest(format!("{DENIED}: read authority is absent")))?;
        let mut file = root.dir.open(&relative).map_err(|e| file_error("read", path, e))?;
        let metadata = file.metadata().map_err(|e| file_error("read", path, e))?;
        if !metadata.is_file() {
            return Err(AutomationError::BadRequest(format!("automation read path is not a regular file: `{path}`")));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| file_error("read", path, e))?;
        Ok(bytes)
    }

    /// Create or replace one file below the configured write root, crash-safely: the bytes go to
    /// a temporary file beside the target, which is synced and renamed over it (the same steps
    /// as [`photocraft_format::atomic_write`], through the directory capability). On failure
    /// the previous file is untouched and the temporary file is removed.
    ///
    /// The parent directory must already exist. `cap-std` performs path
    /// resolution and file creation relative to the held directory handle, so
    /// a non-existent final target is supported without ambient path access.
    pub fn write(&self, path: &str, bytes: &[u8]) -> Result<(), AutomationError> {
        let relative = relative_path(path)?;
        let root = self.write.as_ref().ok_or_else(|| AutomationError::BadRequest(format!("{DENIED}: write authority is absent")))?;
        let leaf = relative.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let parent = relative.parent().map(Path::to_path_buf).unwrap_or_default();
        let tmp = parent.join(photocraft_format::atomic::temp_name(&leaf));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let written = root.dir.open_with(&tmp, &options).and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
        let renamed = written.and_then(|()| photocraft_format::atomic::retry_rename(RenameRetry::platform(), || root.dir.rename(&tmp, &root.dir, &relative)));
        if let Err(e) = renamed {
            let _ = root.dir.remove_file(&tmp);
            return Err(file_error("write", path, e));
        }
        // Flush the directory entry (Unix); best effort, the new bytes are already in place.
        #[cfg(unix)]
        {
            let dir = if parent.as_os_str().is_empty() { PathBuf::from(".") } else { parent };
            if let Ok(d) = root.dir.open(&dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

fn open_root(path: Option<&Path>, authority: &str) -> Result<Option<RootCapability>, AutomationError> {
    let Some(path) = path else { return Ok(None) };
    if path.as_os_str().is_empty() {
        return Err(AutomationError::BadRequest(format!("automation {authority} root is empty")));
    }
    let dir = Dir::open_ambient_dir(path, ambient_authority())
        .map_err(|e| AutomationError::Io(format!("cannot open automation {authority} root `{}`: {e}", path.display())))?;
    Ok(Some(RootCapability { dir: Arc::new(dir) }))
}

fn relative_path(raw: &str) -> Result<PathBuf, AutomationError> {
    if raw.is_empty() {
        return Err(path_error(raw, "path is empty"));
    }
    if raw.contains('\\') {
        return Err(path_error(raw, "alternate separators are not allowed; use `/`"));
    }
    if raw.starts_with('/') {
        return Err(path_error(raw, "absolute paths are not allowed"));
    }
    if raw.contains(':') {
        return Err(path_error(raw, "drive, device and stream prefixes are not allowed"));
    }
    for component in raw.split('/') {
        if component.is_empty() {
            return Err(path_error(raw, "empty path components are not allowed"));
        }
        if component == "." {
            return Err(path_error(raw, "`.` path components are not allowed"));
        }
        if component == ".." {
            return Err(path_error(raw, "parent traversal is not allowed"));
        }
        if component.ends_with(['.', ' ']) {
            return Err(path_error(raw, "path components ending in a dot or space are not allowed"));
        }
        if is_windows_device_name(component) {
            return Err(path_error(raw, "reserved device names are not allowed"));
        }
    }
    Ok(PathBuf::from(raw))
}

fn is_windows_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$")
        || stem.strip_prefix("COM").is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
        || stem.strip_prefix("LPT").is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}

fn path_error(path: &str, reason: &str) -> AutomationError {
    AutomationError::BadRequest(format!("automation path rejected: {reason}: `{path}`"))
}

fn file_error(operation: &str, path: &str, error: std::io::Error) -> AutomationError {
    let kind = error.kind();
    AutomationError::Io(format!("automation {operation} `{path}` failed ({kind:?}): {error}"))
}

/// Filesystem-bearing engine commands have not yet been converted to consume
/// directory capabilities. Automation must use `doc.open`, `doc.save` and
/// `doc.render` for file effects until those commands are migrated.
pub fn authorize_engine_command(id: &str, params: &Value) -> Result<(), AutomationError> {
    let safe_file_command = matches!(
        id,
        "file.new"
            | "file.close"
            | "file.closeAll"
            | "file.closeOthers"
            | "file.fileInfo"
            | "file.automate.fitImage"
            | "file.automate.conditionalModeChange"
            | "file.scripts.flattenAllLayerEffects"
            | "file.scripts.flattenAllMasks"
            | "file.scripts.deleteAllEmptyLayers"
            | "file.export.exportPreferences"
    );
    if id.starts_with("file.") && !safe_file_command {
        return Err(command_error(id));
    }
    if (id.starts_with("layer.smartObjects.") && id != "layer.smartObjects.convertToSmartObject")
        || matches!(
            id,
            "pattern.import"
                | "pattern.export"
                | "edit.presets.migratePresets"
                | "measurementLog.export"
                | "layer.videoLayers.newVideoLayerFromFile"
                | "layer.videoLayers.replaceFootage"
                | "layer.videoLayers.reloadFrame"
                | "image.applyDataSet"
        )
        || profile_command_may_read_ambient(id, params)
        || preferences_may_grant_ambient_paths(id, params)
        || params_contain_ambient_path(id, params)
    {
        return Err(command_error(id));
    }
    Ok(())
}

/// Desktop sessions may already contain user-configured ambient colour-profile
/// paths. Commands which implicitly resolve those settings are denied even
/// when their request parameters contain no path. Fresh headless sessions
/// cannot acquire such settings through the automation interface.
pub fn authorize_desktop_engine_command(id: &str, params: &Value) -> Result<(), AutomationError> {
    authorize_engine_command(id, params)?;
    if id.starts_with("image.mode.") {
        return Err(command_error(id));
    }
    Ok(())
}

fn params_contain_ambient_path(id: &str, params: &Value) -> bool {
    let keys: &[&str] = match id {
        "image.adjustments.colorLookup" | "layer.newAdjustmentLayer.colorLookup" | "layer.setAdjustment" => &["file"],
        "filter.distort.displace" => &["mapPath"],
        "layer.quickExportAsPng" | "layer.exportAs" => &["path"],
        "edit.assignProfile" | "edit.convertToProfile" | "edit.profileInfo" | "view.proofSetup" | "view.gamutWarning" => &["profile"],
        "edit.colorSettings" => &["workingRgb", "workingCmyk", "workingGray"],
        _ => &[],
    };
    keys.iter().any(|key| {
        params
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| if matches!(*key, "file" | "mapPath" | "path") { !value.is_empty() } else { looks_like_path(value) })
    })
}

fn profile_command_may_read_ambient(id: &str, params: &Value) -> bool {
    if matches!(id, "color.profileMismatch" | "edit.colorSettings" | "view.proofSetup") {
        return true;
    }
    matches!(id, "edit.assignProfile" | "edit.convertToProfile" | "edit.profileInfo")
        && params.get("profile").and_then(Value::as_str).is_some_and(|profile| {
            matches!(profile, "working" | "default" | "working-cmyk" | "workingCmyk" | "working-rgb" | "workingRgb" | "working-gray" | "workingGray")
        })
}

fn preferences_may_grant_ambient_paths(id: &str, params: &Value) -> bool {
    if id != "prefs.set" {
        return false;
    }
    let direct = params
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path == "colorSettings" || path.starts_with("colorSettings.") || path == "scriptEvents" || path.starts_with("scriptEvents."));
    let batch = params.get("values").and_then(Value::as_object).is_some_and(|values| {
        values.keys().any(|path| path == "colorSettings" || path.starts_with("colorSettings.") || path == "scriptEvents" || path.starts_with("scriptEvents."))
    });
    direct || batch
}

fn looks_like_path(value: &str) -> bool {
    value.contains('/')
        || value.contains('\\')
        || value.contains(':')
        || value.to_ascii_lowercase().ends_with(".icc")
        || value.to_ascii_lowercase().ends_with(".icm")
}

fn command_error(id: &str) -> AutomationError {
    AutomationError::BadRequest(format!("automation command `{id}` uses ambient filesystem paths and is disabled; use capability-scoped document methods"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(name: &str) -> (PathBuf, PathBuf, AuthorizedWorkspace) {
        let base = std::env::temp_dir().join(format!("photocraft-workspace-{}-{name}", std::process::id()));
        let inside = base.join("inside");
        let outside = base.join("outside");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let workspace = AuthorizedWorkspace::new(Some(&inside), Some(&inside)).unwrap();
        (inside, outside, workspace)
    }

    #[test]
    fn valid_read_and_nonexistent_output_write_stay_in_root() {
        let (inside, _, workspace) = roots("valid");
        std::fs::write(inside.join("read.txt"), b"canary").unwrap();
        assert_eq!(workspace.read("read.txt").unwrap(), b"canary");
        workspace.write("new-output.txt", b"created").unwrap();
        assert_eq!(std::fs::read(inside.join("new-output.txt")).unwrap(), b"created");
    }

    fn temp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect()
    }

    #[test]
    fn write_replaces_atomically_and_leaves_no_temp_file() {
        let (inside, _, workspace) = roots("atomic");
        std::fs::create_dir_all(inside.join("sub")).unwrap();
        workspace.write("sub/out.psd", b"original").unwrap();
        workspace.write("sub/out.psd", b"replacement").unwrap();
        assert_eq!(std::fs::read(inside.join("sub/out.psd")).unwrap(), b"replacement");
        assert!(temp_files(&inside.join("sub")).is_empty());
        // Renaming over a directory fails: nothing is left behind and the directory survives.
        std::fs::create_dir_all(inside.join("adir")).unwrap();
        assert!(workspace.write("adir", b"x").is_err());
        assert!(inside.join("adir").is_dir());
        assert!(temp_files(&inside).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn write_into_read_only_folder_keeps_the_original() {
        use std::os::unix::fs::PermissionsExt;
        let (inside, _, workspace) = roots("readonly");
        let ro = inside.join("ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::write(ro.join("doc.psd"), b"original").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        let root_user = std::fs::File::create(ro.join("probe")).is_ok();
        if !root_user {
            assert!(workspace.write("ro/doc.psd", b"new").is_err());
            assert_eq!(std::fs::read(ro.join("doc.psd")).unwrap(), b"original");
            assert!(temp_files(&ro).is_empty());
        }
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn rejects_absolute_traversal_mixed_prefix_and_malformed_paths_without_panicking() {
        let (_, _, workspace) = roots("reject");
        for path in [
            "",
            "/absolute",
            "../escape",
            "a/../escape",
            "a\\..\\escape",
            "a\\b",
            "C:/escape",
            "a//b",
            "./a",
            "name:stream",
            "NUL",
            "con.txt",
            "folder/COM1.log",
        ] {
            assert!(workspace.read(path).is_err(), "read accepted {path:?}");
            assert!(workspace.write(path, b"x").is_err(), "write accepted {path:?}");
        }
    }

    #[test]
    fn read_and_write_authority_are_separate() {
        let (inside, _, _) = roots("separate");
        std::fs::write(inside.join("read.txt"), b"canary").unwrap();
        let read_only = AuthorizedWorkspace::new(Some(&inside), None).unwrap();
        assert_eq!(read_only.read("read.txt").unwrap(), b"canary");
        assert!(read_only.write("blocked.txt", b"x").is_err());
        let write_only = AuthorizedWorkspace::new(None, Some(&inside)).unwrap();
        assert!(write_only.read("read.txt").is_err());
        write_only.write("written.txt", b"ok").unwrap();
    }

    #[test]
    fn missing_parent_is_a_stable_error_before_any_file_is_created() {
        let (inside, _, workspace) = roots("missing-parent");
        let error = workspace.write("missing/output.txt", b"x").unwrap_err().to_string();
        assert!(error.contains("automation write"), "{error}");
        assert!(!inside.join("missing").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let (inside, outside, workspace) = roots("symlink");
        std::fs::write(outside.join("secret.txt"), b"outside").unwrap();
        symlink(&outside, inside.join("link")).unwrap();
        assert!(workspace.read("link/secret.txt").is_err());
        assert!(workspace.write("link/new.txt", b"blocked").is_err());
        assert!(!outside.join("new.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_symlink_escape_is_rejected_when_supported() {
        use std::os::windows::fs::symlink_dir;

        let (inside, outside, workspace) = roots("windows-link");
        std::fs::write(outside.join("secret.txt"), b"outside").unwrap();
        if symlink_dir(&outside, inside.join("link")).is_err() {
            return;
        }
        assert!(workspace.read("link/secret.txt").is_err());
        assert!(workspace.write("link/new.txt", b"blocked").is_err());
        assert!(!outside.join("new.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_junction_escape_is_rejected_when_supported() {
        use std::process::Command;

        let (inside, outside, workspace) = roots("windows-junction");
        std::fs::write(outside.join("secret.txt"), b"outside").unwrap();
        let link = inside.join("junction");
        let status = Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(&link).arg(&outside).status();
        if !status.is_ok_and(|status| status.success()) {
            return;
        }
        assert!(workspace.read("junction/secret.txt").is_err());
        assert!(workspace.write("junction/new.txt", b"blocked").is_err());
        assert!(!outside.join("new.txt").exists());
        let _ = std::fs::remove_dir(link);
    }

    #[test]
    fn filesystem_commands_fail_closed() {
        for id in [
            "file.openAs",
            "file.export.saveForWebLegacy",
            "pattern.import",
            "layer.smartObjects.exportContents",
            "measurementLog.export",
            "layer.videoLayers.reloadFrame",
            "image.applyDataSet",
            "edit.colorSettings",
        ] {
            assert!(authorize_engine_command(id, &serde_json::json!({})).is_err());
        }
        assert!(authorize_engine_command("file.new", &serde_json::json!({})).is_ok());
        assert!(authorize_engine_command("image.mode.cmyk", &serde_json::json!({})).is_ok());
        assert!(authorize_desktop_engine_command("image.mode.cmyk", &serde_json::json!({})).is_err());
        assert!(authorize_engine_command("filter.distort.displace", &serde_json::json!({"mapPath": "outside.png"})).is_err());
        assert!(authorize_engine_command("layer.setAdjustment", &serde_json::json!({"file": "outside.cube"})).is_err());
        assert!(authorize_engine_command("prefs.set", &serde_json::json!({"path": "colorSettings.workingRgb", "value": "outside.icc"})).is_err());
    }
}
