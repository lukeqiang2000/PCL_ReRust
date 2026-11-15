//! Windows path string helpers, ported from MeloongCore `PathUtils`
//! (Shared/Utils/PathUtils.cs). PCL is a Windows-only launcher, so these
//! operate on Windows-style paths as plain strings, independent of the
//! host OS the port happens to run on.
//!
//! `for_compare` is a string-only approximation of the original: the C#
//! version absolutizes with `Path.GetFullPath` first (which needs a
//! current directory); the port assumes inputs are already absolute and
//! only performs the separator/prefix normalization. Callers comparing
//! relative paths must absolutize them beforehand.

/// MeloongCore `StringUtils.BeforeFirst` analogue for internal use.
pub(crate) fn before_first(s: &str, needle: &str) -> String {
    match s.find(needle) {
        Some(idx) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

/// Text before the last occurrence of `needle`; the whole string when the
/// needle is absent (MeloongCore `BeforeLast`). Kept for parity with the
/// upstream utility surface.
#[allow(dead_code)]
pub(crate) fn before_last(s: &str, needle: &str) -> String {
    match s.rfind(needle) {
        Some(idx) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

/// Text after the last occurrence of any of `needles`
/// (MeloongCore `AfterLastOfAny`).
fn after_last_of_any(s: &str, needles: &[char]) -> String {
    match s.rfind(needles) {
        Some(idx) => s[idx + s[idx..].chars().next().unwrap().len_utf8()..].to_string(),
        None => s.to_string(),
    }
}

/// Text before the last occurrence of any of `needles`
/// (MeloongCore `BeforeLastOfAny`).
fn before_last_of_any(s: &str, needles: &[char]) -> String {
    match s.rfind(needles) {
        Some(idx) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

/// `PathUtils.AddSlashSuffix`: ensure a trailing `\` (either separator
/// counts as already-suffixed, exactly like the original which checks
/// both `DirectorySeparatorChar` and `AltDirectorySeparatorChar`).
pub fn add_slash_suffix(folder: &str) -> String {
    if folder.ends_with('\\') || folder.ends_with('/') {
        folder.to_string()
    } else {
        format!("{folder}\\")
    }
}

/// `PathUtils.RemoveSlashSuffix`: strip all trailing separators.
pub fn remove_slash_suffix(folder: &str) -> String {
    folder.trim_end_matches(['\\', '/']).to_string()
}

/// `PathUtils.RemoveExtendedPrefix`: strip a leading `\\?\` or
/// `\\?\UNC\` prefix.
pub fn remove_extended_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// `PathUtils.ForCompare` (string part): extended prefix removed,
/// `/` unified to `\`, trailing separators removed.
pub fn for_compare(path: &str) -> String {
    remove_slash_suffix(&remove_extended_prefix(path).replace('/', "\\"))
}

/// `PathUtils.IsSame`: null and empty count as the same empty path;
/// otherwise a case-insensitive comparison of the normalized forms.
pub fn is_same_path(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), None) | (None, Some(x)) => x.is_empty(),
        (Some(x), Some(y)) => {
            if x.is_empty() || y.is_empty() {
                return x.is_empty() && y.is_empty();
            }
            for_compare(x).eq_ignore_ascii_case(&for_compare(y))
        }
    }
}

/// `PathUtils.IsParentOf`: child lies strictly below parent, or equals it.
pub fn is_parent_of(parent: &str, child: &str) -> bool {
    let parent = for_compare(parent);
    let child = for_compare(child);
    let prefix = format!("{parent}\\");
    (child.len() >= prefix.len() && child[..prefix.len()].eq_ignore_ascii_case(&prefix))
        || is_same_path(Some(&child), Some(&parent))
}

/// Shared precondition of `RemoveLastPart` / `GetLastPart`: for URLs the
/// query/fragment is cut first.
fn strip_url_suffix(path: &str) -> String {
    let mut p = path.to_string();
    p = before_first(&p, "#");
    p = before_first(&p, "?");
    remove_slash_suffix(&p)
}

/// `PathUtils.RemoveLastPart`: drop the last path segment and any
/// trailing separator; `C:\foo\` becomes `C:`.
pub fn remove_last_part(path: &str) -> String {
    let p = if path.contains("://") {
        strip_url_suffix(path)
    } else {
        remove_slash_suffix(path)
    };
    remove_slash_suffix(&before_last_of_any(&p, &['\\', '/']))
}

/// `PathUtils.GetLastPart`: the last path segment (file or folder name).
pub fn get_last_part(path: &str) -> String {
    let p = if path.contains("://") {
        strip_url_suffix(path)
    } else {
        remove_slash_suffix(path)
    };
    after_last_of_any(&p, &['\\', '/'])
}

/// `PathUtils.ToExtendedFormat`: convert to the `\\?\`-prefixed
/// extended format (trailing separators removed, `/` unified to `\`;
/// UNC paths become `\\?\UNC\...`). Whitespace-only input and paths
/// without a `:` (relative paths) pass through unchanged, as do paths
/// already prefixed.
pub fn to_extended_format(path: &str) -> String {
    if path.trim().is_empty() {
        return path.to_string();
    }
    if !path.contains(':') {
        return path.to_string();
    }
    if path.starts_with(r"\\?\") {
        return path.to_string();
    }
    let normalized = remove_slash_suffix(path).replace('/', "\\");
    if let Some(rest) = normalized.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else {
        format!(r"\\?\{normalized}")
    }
}

/// `PathUtils.ForApi`: the form Windows APIs accept — extended format,
/// with the trailing separator kept only for drive roots (`C:\`).
pub fn for_api(path: &str) -> String {
    let extended = to_extended_format(path);
    if extended.ends_with(':') {
        return format!("{extended}\\");
    }
    if extended.ends_with(r":\") {
        return extended;
    }
    remove_slash_suffix(&extended)
}

// ---------------------------------------------------------------------------
// ToShortPath (MeloongCore PathUtils.ToShortPath).
//
// The string-level algorithm is platform-neutral and ported in full:
// fast returns for short / non-drive paths, the existing file's name
// is preserved, nonexistent tail segments are kept verbatim, and only
// the longest existing ancestor goes through the OS short-name lookup.
// That lookup is the platform hook: GetShortPathNameW under
// cfg(windows), identity everywhere else (macOS/Linux have no 8.3
// short names, and PCL's algorithm is a no-op there in practice —
// their paths carry no drive colon, so the fast return fires first).
//
// One branch of the original is deliberately NOT ported: when even
// the OS short name leaves the combined path over 220 chars, the
// original *creates* 8.3 names on disk (CreateFileW +
// SetFileShortNameW + RtlGenerate8dot3Name) until the path fits.
// That mutates the user's filesystem as a side effect of a "convert
// this path" call; the port returns the combined path as-is in that
// case instead. This is the only observable divergence, and it needs
// a deliberate product decision before it is ever ported.
// ---------------------------------------------------------------------------

/// The buffer size / length limit the original works with (220).
const SHORT_PATH_LIMIT: usize = 220;

#[cfg(windows)]
mod win32 {
    // The single unsafe boundary of the port: the kernel32 calls.
    // Everything outside this module is safe code.
    extern "system" {
        fn GetShortPathNameW(
            lpsz_long_path: *const u16,
            lpsz_short_path: *mut u16,
            cch_buffer: u32,
        ) -> u32;
        fn GetUserDefaultLocaleName(lp_locale_name: *mut u16, cch_locale_name: i32) -> i32;
    }

    /// `GetShortPathNameW` with the original's fixed 220-char buffer:
    /// a return of 0 (failure) or > 220 (would not fit) yields the
    /// input unchanged, exactly like the original's `TryGetShortPath`.
    pub(super) fn get_short_path_name_w(path: &str) -> String {
        let api_path = super::for_api(path);
        let wide: Vec<u16> = api_path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buffer = vec![0u16; super::SHORT_PATH_LIMIT];
        let result = unsafe {
            GetShortPathNameW(
                wide.as_ptr(),
                buffer.as_mut_ptr(),
                super::SHORT_PATH_LIMIT as u32,
            )
        };
        if result == 0 || result > super::SHORT_PATH_LIMIT as u32 {
            return path.to_string();
        }
        String::from_utf16_lossy(&buffer[..result as usize])
    }

    /// `GetUserDefaultLocaleName`: the user's default locale
    /// name (e.g. "zh-CN", "en-US") — the Windows-side locale
    /// source upstream PCL's environment checks effectively run
    /// under. `None` on failure or an empty name.
    pub(super) fn get_user_default_locale_name() -> Option<String> {
        // LOCALE_NAME_MAX_LENGTH is 85.
        let mut buffer = vec![0u16; 85];
        let result =
            unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
        if result <= 0 {
            return None;
        }
        let written = (result as usize).min(buffer.len());
        let name = String::from_utf16_lossy(&buffer[..written]);
        let name = name.trim_end_matches('\0');
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }
}

/// The platform short-name lookup used by [`to_short_path`]:
/// GetShortPathNameW on Windows, identity on macOS/Linux.
#[cfg(windows)]
fn platform_get_short_path(path: &str) -> String {
    win32::get_short_path_name_w(path)
}

/// See [`platform_get_short_path`].
#[cfg(not(windows))]
fn platform_get_short_path(path: &str) -> String {
    path.to_string()
}

/// The user's default locale name from the OS
/// (`GetUserDefaultLocaleName` on Windows, e.g. "zh-CN");
/// `None` where the platform hook has no equivalent.
#[cfg(windows)]
pub fn user_default_locale_name() -> Option<String> {
    win32::get_user_default_locale_name()
}

/// See [`user_default_locale_name`].
#[cfg(not(windows))]
pub fn user_default_locale_name() -> Option<String> {
    None
}

/// Windows `Path.GetDirectoryName` for the string shapes this module
/// handles: the parent of `C:\a\b` is `C:\a`, of `C:\a` is `C:\`, of
/// `C:\` (or `C:`) is None — the original stops there via its
/// empty/same checks.
fn parent_dir(path: &str) -> Option<String> {
    let trimmed = remove_slash_suffix(path);
    let idx = trimmed.rfind(['\\', '/'])?;
    if idx == 0 {
        return None;
    }
    let parent = &trimmed[..idx];
    if parent.ends_with(':') {
        // "C:\a" -> "C:\" (GetDirectoryName keeps the root separator).
        Some(format!("{parent}\\"))
    } else {
        Some(parent.to_string())
    }
}

/// Path combination with `\`, mirroring `Path.Combine` for the
/// already-normalized shapes used here (an empty tail returns the
/// head unchanged).
fn combine_path(head: &str, tail: &str) -> String {
    if tail.is_empty() {
        return head.to_string();
    }
    if head.is_empty() {
        return tail.to_string();
    }
    if head.ends_with('\\') || head.ends_with('/') {
        format!("{head}{tail}")
    } else {
        format!("{head}\\{tail}")
    }
}

/// `PathUtils.ToShortPath` with the platform hooks injected (the
/// short-name lookup plus the file/directory existence probes,
/// mirroring the original's `FileUtils.Exists` /
/// `DirectoryUtils.Exists` calls), so the algorithm can be tested on
/// any OS. [`to_short_path`] is the production entry point using the
/// real hooks.
///
/// Notes on faithfulness:
/// - Lengths are UTF-16 code-unit counts, as in the original
///   (C# `string.Length`).
/// - The original's `ForCompare` step absolutizes with
///   `Path.GetFullPath`; this module's string-only `for_compare`
///   keeps the batch-1 convention that inputs are already absolute.
/// - The 8.3-name *generation* fallback is not ported (see the
///   section comment above).
pub fn to_short_path_with(
    path: &str,
    get_short: &dyn Fn(&str) -> String,
    file_exists: &dyn Fn(&str) -> bool,
    dir_exists: &dyn Fn(&str) -> bool,
) -> String {
    let utf16_len = |s: &str| s.encode_utf16().count();
    // Fast returns.
    if path.is_empty() {
        return path.to_string();
    }
    if !path.contains(':') || path.contains("://") {
        return path.to_string();
    }
    if utf16_len(path) <= SHORT_PATH_LIMIT {
        return path.to_string();
    }
    let normalized = for_compare(path);
    // Preserve the original file name where possible: only the
    // directory part is shortened.
    let mut path_to_keep = String::new();
    let mut path_to_shorten = normalized.clone();
    if file_exists(&normalized) {
        let file_name = get_last_part(&normalized);
        if utf16_len(&file_name) < 180 {
            path_to_keep = file_name;
            path_to_shorten = remove_last_part(&normalized);
        }
        // A file name of 180+ chars: the original logs a warning and
        // shortens the whole path including the name (path_to_keep
        // stays empty).
    }
    // Walk up to the longest existing ancestor; the nonexistent tail
    // moves into path_to_keep verbatim.
    while !dir_exists(&path_to_shorten) && !file_exists(&path_to_shorten) {
        let Some(parent) = parent_dir(&path_to_shorten) else {
            return normalized;
        };
        if is_same_path(Some(&parent), Some(&path_to_shorten)) {
            return normalized;
        }
        path_to_keep = combine_path(&get_last_part(&path_to_shorten), &path_to_keep);
        path_to_shorten = parent;
    }
    if utf16_len(&path_to_keep) >= 200 {
        // Most of the path does not exist yet; cannot shorten.
        return normalized;
    }
    let short_path = get_short(&path_to_shorten);
    // (The original would now try generating 8.3 names if the
    // combined path is still over the limit — not ported, see the
    // section comment.)
    remove_slash_suffix(&remove_extended_prefix(&combine_path(
        &short_path,
        &path_to_keep,
    )))
}

/// `PathUtils.ToShortPath`: shorten an over-long Windows path via
/// the OS 8.3 short names, keeping the result at or under ~220 chars
/// where the OS cooperates. On macOS/Linux the lookup is the
/// identity, so this is a pass-through apart from the algorithm's
/// normalization steps.
pub fn to_short_path(path: &str) -> String {
    to_short_path_with(
        path,
        &platform_get_short_path,
        &|p| std::path::Path::new(p).is_file(),
        &|p| std::path::Path::new(p).is_dir(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_suffixes() {
        assert_eq!(add_slash_suffix(r"C:\Java"), r"C:\Java\");
        assert_eq!(add_slash_suffix(r"C:\Java\"), r"C:\Java\");
        assert_eq!(add_slash_suffix("C:/Java/"), "C:/Java/");
        assert_eq!(remove_slash_suffix(r"C:\Java\\"), r"C:\Java");
    }

    #[test]
    fn last_part_matches_original_examples() {
        // Examples taken from the original XML doc comments.
        assert_eq!(remove_last_part(r"C:\foo\bar.txt"), r"C:\foo");
        assert_eq!(remove_last_part(r"C:\foo\bar\"), r"C:\foo");
        assert_eq!(remove_last_part(r"C:\foo\"), "C:");
        assert_eq!(
            remove_last_part("https://foo.bar/file/pack.zip?arg=1"),
            "https://foo.bar/file"
        );
        assert_eq!(get_last_part(r"C:\foo\bar.txt"), "bar.txt");
        assert_eq!(get_last_part(r"C:\foo\bar\"), "bar");
        assert_eq!(
            get_last_part("https://foo.bar/file/pack.zip?arg=1"),
            "pack.zip"
        );
    }

    #[test]
    fn compare_and_parent() {
        assert!(is_same_path(Some(r"c:\JAVA\bin\"), Some("C:/Java/bin")));
        assert!(is_same_path(None, Some("")));
        assert!(!is_same_path(Some(r"C:\a"), Some(r"C:\ab")));
        assert!(is_parent_of(r"C:\Java", r"C:\Java\jdk-21\bin"));
        assert!(is_parent_of(r"C:\Java", r"C:\Java"));
        assert!(!is_parent_of(r"C:\Java", r"C:\JavaOther\bin"));
    }

    #[test]
    fn extended_prefix() {
        assert_eq!(remove_extended_prefix(r"\\?\C:\foo"), r"C:\foo");
        assert_eq!(remove_extended_prefix(r"\\?\UNC\srv\sh"), r"\\srv\sh");
        assert_eq!(for_compare(r"\\?\C:/foo/"), r"C:\foo");
    }

    #[test]
    fn extended_format_and_for_api() {
        assert_eq!(to_extended_format(""), "");
        assert_eq!(to_extended_format("relative\\path"), "relative\\path");
        assert_eq!(to_extended_format(r"C:\a\b\"), r"\\?\C:\a\b");
        assert_eq!(to_extended_format("C:/a/b"), r"\\?\C:\a\b");
        assert_eq!(to_extended_format(r"\\?\C:\a"), r"\\?\C:\a");
        // Quirk of the original: a UNC path WITHOUT a colon hits the
        // early "no colon" return and never reaches the UNC branch;
        // the branch only fires when a ':' appears somewhere.
        assert_eq!(
            to_extended_format(r"\\server\share\dir"),
            r"\\server\share\dir"
        );
        assert_eq!(
            to_extended_format(r"\\server\share:alt\dir"),
            r"\\?\UNC\server\share:alt\dir"
        );
        assert_eq!(for_api(r"C:\"), r"\\?\C:\");
        assert_eq!(for_api(r"C:\a\"), r"\\?\C:\a");
    }

    /// Fake shortener mimicking 8.3 names: every directory segment
    /// longer than 8 chars collapses to `xxxxxx~1`, like real
    /// GetShortPathNameW output.
    fn fake_short(path: &str) -> String {
        path.split('\\')
            .map(|seg| {
                if seg.len() > 9 && !seg.contains(':') {
                    seg[..6].to_string() + "~1"
                } else {
                    seg.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\\")
    }

    /// `C:\` + five 50-char segments + `game.jar` = 266 chars.
    fn long_path() -> (String, String) {
        let seg = "d".repeat(50);
        let full = format!(r"C:\{0}\{0}\{0}\{0}\{0}\game.jar", seg);
        (full, seg)
    }

    #[test]
    fn short_path_fast_returns() {
        let none = |_: &str| false;
        // Short path: untouched.
        assert_eq!(
            to_short_path_with(r"C:\a\b.jar", &fake_short, &none, &none),
            r"C:\a\b.jar"
        );
        // URL: untouched even when long.
        let long_url = format!("https://example.com/{}", "a".repeat(300));
        assert_eq!(to_short_path_with(&long_url, &fake_short, &none, &none), long_url);
        // No drive colon: untouched even when long.
        let long_rel = "a\\".repeat(150);
        assert_eq!(to_short_path_with(&long_rel, &fake_short, &none, &none), long_rel);
    }

    #[test]
    fn short_path_shortens_existing_ancestors_keeps_tail() {
        let (full, seg) = long_path();
        let at_depth = |n: usize| {
            let mut s = "C:".to_string();
            for _ in 0..n {
                s += "\\";
                s += &seg;
            }
            s
        };
        // The file exists; directories exist down to depth 3 only.
        let file_exists = |p: &str| p == full;
        let dirs = [at_depth(1), at_depth(2), at_depth(3)];
        let dir_exists = |p: &str| dirs.iter().any(|d| d == p);
        let out = to_short_path_with(&full, &fake_short, &file_exists, &dir_exists);
        // Depths 1-3 go through the shortener; the nonexistent
        // depths 4-5 and the file name are preserved verbatim.
        let expected = format!(
            r"C:\dddddd~1\dddddd~1\dddddd~1\{0}\{0}\game.jar",
            seg
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn short_path_gives_up_on_long_nonexistent_tail() {
        let (full, _) = long_path();
        let none = |_: &str| false;
        // Only the root exists: the kept tail is >= 200 chars, so
        // the normalized original comes back.
        let root_only = |p: &str| p == "C:\\";
        assert_eq!(
            to_short_path_with(&full, &fake_short, &none, &root_only),
            for_compare(&full)
        );
        // Nothing exists at all: the walk hits the root's missing
        // parent and likewise returns the original.
        assert_eq!(
            to_short_path_with(&full, &fake_short, &none, &none),
            for_compare(&full)
        );
    }
}
