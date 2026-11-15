//! Platform facts for the UI layer (M3 batch 1).
//!
//! Everything OS-specific in `pcl-ui` lives here or behind
//! `#[cfg]` in this module: candidate `.minecraft` locations and
//! Java search roots. The controller only consumes `PathBuf`s, so
//! no Windows/macOS/Linux assumption leaks into the launch chain
//! (PORT-PLAN §8: platform differences go through cfg/abstraction).

use std::path::PathBuf;

#[cfg(not(target_os = "windows"))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Candidate `.minecraft` folders, most likely first. Mirrors where
/// PCL/HMCL users keep the folder on each OS; the controller picks
/// the first candidate that actually has a `versions` directory.
pub fn mc_dir_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            out.push(PathBuf::from(appdata).join(".minecraft"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            out.push(home.join("Library").join("Application Support").join("minecraft"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = home_dir() {
            out.push(home.join(".minecraft"));
        }
    }
    // A `.minecraft` sitting next to the executable or in the
    // working directory (how the user's own PCL folder is laid out).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(".minecraft"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(".minecraft"));
    }
    out.dedup();
    out
}

/// Roots under which Java runtimes are searched (depth-limited
/// recursion happens in the controller, like the CLI's discovery).
pub fn java_search_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(java_home) = std::env::var_os("JAVA_HOME") {
        out.push(PathBuf::from(java_home));
    }
    #[cfg(target_os = "windows")]
    {
        for base in [r"C:\Program Files\Java", r"C:\Program Files\Eclipse Adoptium", r"C:\Program Files\Microsoft"] {
            out.push(PathBuf::from(base));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            // HMCL-managed runtimes live under .hmcl\java\<platform>\<runtime>\bin.
            out.push(PathBuf::from(appdata).join(".hmcl").join("java"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        if let Some(home) = home_dir() {
            out.push(home.join(".sdkman").join("candidates").join("java"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        out.push(PathBuf::from("/usr/lib/jvm"));
        if let Some(home) = home_dir() {
            out.push(home.join(".sdkman").join("candidates").join("java"));
        }
    }
    out
}

/// The Java executable file name on this OS.
pub fn java_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_not_empty() {
        assert!(!mc_dir_candidates().is_empty());
        assert!(!java_search_roots().is_empty());
    }
}
