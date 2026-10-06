//! The main display's ICC profile, for the colour-managed canvas (Edit › Color Settings ›
//! Monitor Profile = `auto`).
//!
//! macOS: `NSScreen.mainScreen.colorSpace.ICCProfileData`, a documented AppKit API, read through
//! `osascript` (AppleScriptObjC) so the app needs no `unsafe` FFI. The query runs on a background
//! thread at launch (about 0.4 s) and the profile is applied when it arrives. Other platforms
//! return `None` (sRGB, or the profile chosen in Color Settings).

use std::sync::mpsc::Receiver;

/// Starts reading the main display's profile in the background.
pub fn detect_async() -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(detect());
    });
    rx
}

#[cfg(target_os = "macos")]
fn detect() -> Option<Vec<u8>> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "use framework \"AppKit\"",
            "-e",
            "return ((current application's NSScreen's mainScreen()'s colorSpace()'s ICCProfileData()'s base64EncodedStringWithOptions:0) as text)",
        ])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let bytes = base64_decode(std::str::from_utf8(&out.stdout).ok()?.trim())?;
    // An ICC profile starts with its size and carries `acsp` at offset 36.
    (bytes.len() >= 132 && bytes.get(36..40) == Some(b"acsp")).then_some(bytes)
}

#[cfg(not(target_os = "macos"))]
fn detect() -> Option<Vec<u8>> {
    None
}

/// Standard base64 (RFC 4648, with padding) → bytes; `None` on any invalid character.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            acc |= val(*c)? << (18 - 6 * i);
        }
        let n = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        out.extend_from_slice(&acc.to_be_bytes()[1..1 + n]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64() {
        assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64_decode("aGVsbG8h").as_deref(), Some(&b"hello!"[..]));
        assert_eq!(base64_decode("aGk=").as_deref(), Some(&b"hi"[..]));
        assert_eq!(base64_decode("").as_deref(), Some(&b""[..]));
        assert!(base64_decode("a").is_none());
        assert!(base64_decode("a$==").is_none());
    }

    /// The detected profile (when there is one) parses as an RGB profile.
    #[test]
    fn detected_profile_parses() {
        if let Some(bytes) = detect() {
            let p = photocraft_engine::color_cmds::profile_from_bytes(&std::sync::Arc::new(bytes)).expect("parses");
            assert_eq!(format!("{:?}", p.color_space), "Rgb", "{}", p.description);
        }
    }
}
