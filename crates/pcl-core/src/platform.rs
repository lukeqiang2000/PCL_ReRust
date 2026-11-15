//! Host-platform facts for the cross-platform port (PORT-PLAN §8).
//!
//! PCL itself is Windows-only, so every place the port must behave
//! differently per OS funnels through here (or through `cfg` at the
//! call site): the Mojang rules/natives OS key, the native-library
//! file extension extracted from natives jars, and the `os.version`
//! string fed to rule evaluation.

/// The Mojang vocabulary OS key of the host: "windows" / "osx" /
/// "linux".
pub fn host_os_key() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

/// Native library extension extracted from natives jars
/// (`McLaunchNatives` extracts `.dll` on Windows; the port takes
/// the host's extension).
pub fn natives_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

/// The string rule evaluation matches `os.version` regexes against.
/// PCL reads `My.Computer.Info.OSVersion` ("Microsoft Windows NT
/// 10.0.…"); the port reports the same shape on Windows builds and
/// an empty string elsewhere (version-gated rules then simply do
/// not match, which only affects Windows-version-gated entries).
pub fn host_os_version() -> String {
    #[cfg(target_os = "windows")]
    {
        // The exact build number is not queryable without Win32;
        // PCL-era manifests only anchor on "^10\\." / "^6\\.", so
        // the NT 10.0 shape is the faithful approximation.
        "Microsoft Windows NT 10.0.0.0".to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        String::new()
    }
}

/// Whether the host OS is 64-bit (feeds `${arch}` → "64"/"32").
pub fn host_is_64bit() -> bool {
    cfg!(target_pointer_width = "64")
}

/// The user's locale name (e.g. "zh-CN") as reported by the OS
/// itself: `GetUserDefaultLocaleName` on Windows, through the
/// port's Win32 boundary in `pathutil`. Upstream PCL runs under
/// the Windows system locale; a port that reads only the POSIX
/// locale environment (LC_ALL/LANG) sees an empty string under a
/// plain Windows cmd and misjudges the environment (the M2
/// real-machine run had to fake `LANG=zh_CN.UTF-8` to get past
/// the non-Chinese demo gate). `None` on other platforms, where
/// callers fall back to the locale environment as before.
pub fn user_locale_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        crate::pathutil::user_default_locale_name()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}
