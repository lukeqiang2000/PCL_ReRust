//! Java discovery and requirement logic.
//!
//! Sources: `PCLCS/Java.cs` (the `Java` class and `JavaUtils`) and
//! `Plain Craft Launcher 2/Modules/Minecraft/ModJava.vb`
//! (`GetJavaRequirement`, the Mojang runtime-list parsing and the
//! manual-install prompt text). Process execution and directory walking
//! sit behind small injectable seams (`JavaRunner`, explicit root
//! lists); everything else is pure logic ported as-is.

use crate::pathutil;
use crate::vrange::{
    compare_version, compare_version_ge, JavaVersion, ValueRange,
};
use std::collections::HashMap;
use std::io;
use std::time::Duration;

// ---------------------------------------------------------------------------
// ModLoaders — PCLCS Constants.cs flag enum.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ModLoaders(pub u32);

impl ModLoaders {
    pub const NONE: Self = Self(0);
    pub const FORGE: Self = Self(1);
    pub const LITELOADER: Self = Self(2);
    pub const FABRIC: Self = Self(4);
    pub const NEOFORGE: Self = Self(16);
    pub const ALL: Self = Self(1 | 2 | 4 | 16);

    pub fn has(self, other: ModLoaders) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for ModLoaders {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

// ---------------------------------------------------------------------------
// Java probe: parsing of `java -version` output (PCLCS Java.CheckAsync).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaProbe {
    pub version: JavaVersion,
    pub file_encoding: Option<String>,
    pub native_encoding: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaCheckError {
    EmptyOutput,
    LibExtExists,
    FatalError,
    VersionNotFound,
    BadVersion(String),
    Not64Bit,
}

impl std::fmt::Display for JavaCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyOutput => write!(f, "尝试运行该 Java 失败"),
            Self::LibExtExists => {
                write!(f, "无法运行该 Java，请在删除 Java 文件夹中的 /lib/ext 文件夹后再试")
            }
            Self::FatalError => write!(f, "无法运行该 Java，该 Java 或系统存在问题"),
            Self::VersionNotFound => write!(f, "未找到该 Java 的版本号"),
            Self::BadVersion(v) => write!(f, "分析详细信息失败，获取的版本为 {v}"),
            Self::Not64Bit => write!(f, "该 Java 为 32 位版本，请安装 64 位的 Java"),
        }
    }
}

/// Capture the text after `needle` up to the next `"`.
fn seek_quoted(output: &str, needle: &str) -> Option<String> {
    let start = output.find(needle)? + needle.len();
    let rest = &output[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Capture the leading digit run after `needle`.
fn seek_digits(output: &str, needle: &str) -> Option<String> {
    let start = output.find(needle)? + needle.len();
    let digits: String = output[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

fn get_property(output: &str, name: &str) -> Option<String> {
    let prefix = format!("{name} = ");
    output
        .split('\n')
        .map(|l| l.trim())
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim().to_string())
}

/// Pure port of the output-parsing half of PCLCS `Java.CheckAsync`.
/// The original lowercases the *entire* program output before looking at
/// it, so the returned encoding names are lowercase as well — kept.
pub fn parse_check_output(raw_output: &str) -> Result<JavaProbe, JavaCheckError> {
    let output = raw_output.to_lowercase();
    if output.is_empty() {
        return Err(JavaCheckError::EmptyOutput);
    }
    if output.contains("/lib/ext exists") {
        return Err(JavaCheckError::LibExtExists);
    }
    if output.contains("a fatal error") || output.contains("error: ") {
        return Err(JavaCheckError::FatalError);
    }
    // Version string: (?<=version ")[^"]+ , falling back to (?<=openjdk )[0-9]+
    let found = seek_quoted(&output, "version \"")
        .or_else(|| seek_digits(&output, "openjdk "));
    let Some(found) = found else {
        return Err(JavaCheckError::VersionNotFound);
    };
    let mut version_string = found.replace('_', ".").replace('+', ".");
    if let Some(idx) = version_string.find('-') {
        version_string.truncate(idx);
    }
    if version_string.is_empty() {
        return Err(JavaCheckError::VersionNotFound);
    }
    if let Some(rest) = version_string.strip_prefix("1.") {
        version_string = rest.to_string();
    }
    let mut segments: Vec<&str> = version_string.split('.').filter(|s| !s.is_empty()).collect();
    if segments.len() > 4 {
        return Err(JavaCheckError::BadVersion(version_string.clone()));
    }
    while segments.len() < 4 {
        segments.push("0");
    }
    let mut nums = [0i64; 4];
    for (i, seg) in segments.iter().enumerate() {
        nums[i] = seg
            .parse::<i64>()
            .map_err(|_| JavaCheckError::BadVersion(version_string.clone()))?;
    }
    let version = JavaVersion::dotnet4(nums[0], nums[1], nums[2], nums[3]);
    if version.major <= 4 || version.major >= 100 {
        return Err(JavaCheckError::BadVersion(version.to_string()));
    }
    if !output.contains("64-bit") {
        return Err(JavaCheckError::Not64Bit);
    }
    let file_encoding = get_property(&output, "file.encoding");
    let native_encoding = get_property(&output, "native.encoding").or_else(|| file_encoding.clone());
    Ok(JavaProbe { version, file_encoding, native_encoding })
}

// ---------------------------------------------------------------------------
// Java — PCLCS Java class. Equality is by folder, case-insensitively.
// ---------------------------------------------------------------------------

/// Runs a program and returns its combined output. The platform seam
/// behind `Java::check` (PCL uses its own async process runner with a
/// 15 s timeout).
pub trait JavaRunner {
    fn run(&self, exe_path: &str, args: &[&str], timeout: Duration) -> io::Result<String>;
}

/// `JavaRunner` backed by `std::process`, merging stderr and stdout
/// (Java prints `-version` and `-XshowSettings` output to stderr).
pub struct ProcessJavaRunner;

impl JavaRunner for ProcessJavaRunner {
    fn run(&self, exe_path: &str, args: &[&str], timeout: Duration) -> io::Result<String> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let mut child = Command::new(exe_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut out_pipe = child.stdout.take().expect("stdout piped");
        let mut err_pipe = child.stderr.take().expect("stderr piped");
        let out_thread = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = out_pipe.read_to_string(&mut s);
            s
        });
        let err_thread = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err_pipe.read_to_string(&mut s);
            s
        });
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait()? {
                Some(_) => break,
                None => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "java probe timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        let stdout = out_thread.join().unwrap_or_default();
        let stderr = err_thread.join().unwrap_or_default();
        Ok(format!("{stderr}{stdout}"))
    }
}

#[derive(Debug, Clone)]
pub struct Java {
    /// Folder containing java.exe, normalized and ending with `\`.
    pub folder: String,
    pub version: Option<JavaVersion>,
    pub file_encoding: Option<String>,
    pub native_encoding: Option<String>,
    checked: bool,
    available: Option<bool>,
}

impl Java {
    pub fn new(folder: &str) -> Java {
        Java {
            folder: pathutil::add_slash_suffix(&pathutil::for_compare(folder)),
            version: None,
            file_encoding: None,
            native_encoding: None,
            checked: false,
            available: None,
        }
    }

    pub fn with_version(folder: &str, version: JavaVersion) -> Java {
        let mut j = Java::new(folder);
        j.version = Some(version);
        j
    }

    pub fn exe_path(&self) -> String {
        format!("{}java.exe", self.folder)
    }

    pub fn is_available(&self) -> Option<bool> {
        self.available
    }

    pub fn invalidate_checked(&mut self) {
        self.checked = false;
        self.available = None;
    }

    /// PCLCS `CheckAsync`: probes the executable once and caches the
    /// outcome. A runner timeout mirrors the original's cancellation
    /// path: the checked flag is reset so a later call retries.
    pub fn check(&mut self, runner: &dyn JavaRunner) -> bool {
        if self.checked {
            if let Some(available) = self.available {
                return available;
            }
        }
        self.checked = true;
        let exe = self.exe_path();
        // The original checks File.Exists first (FileNotFoundException
        // -> check fails); a spawn failure lands in the same arm below.
        match runner.run(&exe, &["-XshowSettings:properties", "-version"], Duration::from_millis(15_000)) {
            Ok(output) => match parse_check_output(&output) {
                Ok(probe) => {
                    self.version = Some(probe.version);
                    self.file_encoding = probe.file_encoding;
                    self.native_encoding = probe.native_encoding;
                    self.available = Some(true);
                }
                Err(_) => {
                    self.available = Some(false);
                }
            },
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                self.checked = false;
                self.available = None;
                return false;
            }
            Err(_) => {
                self.available = Some(false);
            }
        }
        self.available.unwrap_or(false)
    }

    /// PCLCS `ToString`: `Java {major or 尚未检查} ({version})：{folder}`.
    pub fn display(&self) -> String {
        let major = self
            .version
            .map(|v| v.major.to_string())
            .unwrap_or_else(|| "尚未检查".to_string());
        let version = self.version.map(|v| v.to_string()).unwrap_or_default();
        format!("Java {major} ({version})：{}", self.folder)
    }
}

impl PartialEq for Java {
    fn eq(&self, other: &Self) -> bool {
        pathutil::is_same_path(Some(&self.folder), Some(&other.folder))
    }
}
impl Eq for Java {}
impl std::hash::Hash for Java {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        pathutil::for_compare(&self.folder).to_lowercase().hash(state);
    }
}

/// `JavaUtils.CheckAllAsync`: check every not-yet-checked Java and keep
/// the ones that pass, in input order (the original checks in parallel;
/// ordering of the result is the same).
pub fn check_all(javas: &mut [Java], runner: &dyn JavaRunner) -> Vec<Java> {
    for java in javas.iter_mut() {
        if java.is_available().is_none() {
            java.check(runner);
        }
    }
    javas
        .iter()
        .filter(|j| j.is_available() == Some(true))
        .cloned()
        .collect()
}

/// `JavaUtils.SortAsync` ordering (after `CheckAllAsync`): Java under a
/// candidate folder first, then by distance of the major version to 21.
/// The C# `List.Sort` is not stable; the port's stable sort may order
/// exact ties differently.
pub fn sort_javas(javas: &mut [Java], candidate_folders: &[String]) {
    let in_candidate = |j: &Java| {
        candidate_folders
            .iter()
            .any(|f| pathutil::is_parent_of(f, &j.folder))
    };
    javas.sort_by(|left, right| {
        let (lc, rc) = (in_candidate(left), in_candidate(right));
        if lc != rc {
            return if lc { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
        }
        let ld = left.version.map(|v| (v.major - 21).abs()).unwrap_or(i64::MAX);
        let rd = right.version.map(|v| (v.major - 21).abs()).unwrap_or(i64::MAX);
        ld.cmp(&rd)
    });
}

// ---------------------------------------------------------------------------
// Candidate folders & search (JavaUtils.CandidateFolders / SearchFolders).
// ---------------------------------------------------------------------------

/// Base directories the candidate-folder list is built from. All values
/// are plain Windows paths *without* a trailing separator.
#[derive(Debug, Clone, Default)]
pub struct JavaCandidateEnv {
    pub app_data: String,
    pub user_profile: String,
    pub local_app_data: String,
    pub program_files: String,
    pub program_files_x86: String,
    pub my_documents: String,
    pub jdk_home: Option<String>,
    pub java_home: Option<String>,
    /// Full paths of the `jdk-*` directories under
    /// `{ProgramFiles}\Microsoft\` (enumerated by the platform layer;
    /// the original globs them inline).
    pub microsoft_jdk_dirs: Vec<String>,
}

/// `JavaUtils.CandidateFolders`: the fixed search roots, in the
/// original's order, each normalized with `for_compare`.
pub fn candidate_folders(env: &JavaCandidateEnv) -> Vec<String> {
    let mut folders: Vec<String> = vec![
        format!(r"{}\.minecraft\runtime", env.app_data),
        format!(r"{}\.hmcl\java", env.app_data),
        format!(r"{}\ATLauncher\runtimes\minecraft", env.app_data),
        format!(r"{}\ModrinthApp\meta\java_versions", env.app_data),
        format!(r"{}\PrismLauncher\java", env.app_data),
        format!(r"{}\curseforge\minecraft\Install\runtime", env.user_profile),
        format!(r"{}\.jdks", env.user_profile),
        format!(r"{}\.sdkman\candidates\java", env.user_profile),
        format!(r"{}\.ftba\bin\runtime", env.local_app_data),
        format!(
            r"{}\Packages\Microsoft.4297127D64EC6_8wekyb3d8bbwe\LocalCache\Local\runtime",
            env.local_app_data
        ),
        format!(r"{}\Minecraft Launcher\runtime", env.program_files_x86),
        format!(r"{}\Minecraft\runtime", env.program_files_x86),
        format!(r"{}\Curse\Minecraft\Install\runtime", env.my_documents),
        format!(r"{}\Java", env.program_files),
        format!(r"{}\Eclipse Adoptium", env.program_files),
        format!(r"{}\Amazon Corretto", env.program_files),
        format!(r"{}\Zulu", env.program_files),
    ];
    folders.extend(env.microsoft_jdk_dirs.iter().cloned());
    // JDK_HOME + JAVA_HOME, ';'-separated, entries trimmed of spaces/quotes.
    let env_joined = format!(
        "{};{}",
        env.jdk_home.as_deref().unwrap_or(""),
        env.java_home.as_deref().unwrap_or("")
    );
    for entry in env_joined.split(';').filter(|s| !s.is_empty()) {
        folders.push(entry.trim_matches(|c| c == ' ' || c == '"').to_string());
    }
    folders
        .iter()
        .map(|f| pathutil::for_compare(f))
        .filter(|f| !f.is_empty())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaSearchHit {
    pub folder: String,
    pub has_reparse_point: bool,
    pub is_special_path: bool,
}

/// The special-path markers from `SearchFoldersAsync` (matched
/// case-insensitively against the Java's folder).
pub fn is_special_path(folder: &str) -> bool {
    let f = folder.to_lowercase();
    ["java8path_target_", "javapath_target_", "javatmp", "system32"]
        .iter()
        .any(|m| f.contains(m))
}

/// Whether the path or any ancestor is a reparse point (symlink /
/// junction). Paths under `exempt_prefix` (PCL's own runtime folder,
/// #8928) are never flagged.
pub fn has_reparse_point(exe_path: &str, exempt_prefix: Option<&str>) -> bool {
    if let Some(prefix) = exempt_prefix {
        if pathutil::is_parent_of(prefix, exe_path) {
            return false;
        }
    }
    let path = std::path::Path::new(exe_path);
    let mut current = Some(path);
    while let Some(p) = current {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            if meta.file_type().is_symlink() {
                return true;
            }
        }
        current = p.parent();
    }
    false
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilterOutcome {
    /// Surviving folders, deduplicated case-insensitively, in
    /// first-seen order.
    pub folders: Vec<String>,
    pub reparse_dropped: Vec<String>,
    pub special_dropped: Vec<String>,
    /// All hits had reparse points, so all were kept (original logs a
    /// warning and keeps everything).
    pub all_reparse_kept: bool,
    pub all_special_kept: bool,
}

/// The two "drop the flagged ones unless *everything* is flagged"
/// filters of `SearchFoldersAsync`, applied in the original's order
/// (reparse first, then special paths on the survivors).
pub fn filter_search_hits(hits: Vec<JavaSearchHit>) -> SearchFilterOutcome {
    let mut outcome = SearchFilterOutcome::default();
    if hits.is_empty() {
        return outcome;
    }
    let mut current = hits;
    if current.iter().all(|h| h.has_reparse_point) {
        outcome.all_reparse_kept = true;
    } else if current.iter().any(|h| h.has_reparse_point) {
        outcome.reparse_dropped = current
            .iter()
            .filter(|h| h.has_reparse_point)
            .map(|h| h.folder.clone())
            .collect();
        current.retain(|h| !h.has_reparse_point);
    }
    if current.iter().all(|h| h.is_special_path) {
        outcome.all_special_kept = true;
    } else if current.iter().any(|h| h.is_special_path) {
        outcome.special_dropped = current
            .iter()
            .filter(|h| h.is_special_path)
            .map(|h| h.folder.clone())
            .collect();
        current.retain(|h| !h.is_special_path);
    }
    let mut seen: Vec<String> = Vec::new();
    for hit in current {
        let key = pathutil::for_compare(&hit.folder).to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            outcome.folders.push(hit.folder);
        }
    }
    outcome
}

/// Walk the given roots for `java.exe` files (recursively when
/// `include_subdirs`), building the hit list `SearchFoldersAsync`
/// starts from. Unreadable roots are skipped, as in the original.
pub fn collect_search_hits(
    roots: &[String],
    include_subdirs: bool,
    runtime_exempt_prefix: Option<&str>,
) -> Vec<JavaSearchHit> {
    let mut hits = Vec::new();
    let mut seen_roots: Vec<String> = Vec::new();
    for root in roots {
        let key = root.to_lowercase();
        if seen_roots.contains(&key) {
            continue;
        }
        seen_roots.push(key);
        let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(root)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else { continue };
                if file_type.is_dir() {
                    if include_subdirs {
                        stack.push(path);
                    }
                } else if entry.file_name().to_string_lossy().eq_ignore_ascii_case("java.exe") {
                    let exe = path.to_string_lossy().to_string();
                    let folder = pathutil::add_slash_suffix(&pathutil::for_compare(
                        &pathutil::remove_last_part(&exe),
                    ));
                    hits.push(JavaSearchHit {
                        has_reparse_point: has_reparse_point(&exe, runtime_exempt_prefix),
                        is_special_path: is_special_path(&folder),
                        folder,
                    });
                }
            }
        }
    }
    hits
}

/// `JavaUtils.SearchFoldersAsync`: collect, filter, check, sort.
pub fn search_folders(
    roots: &[String],
    include_subdirs: bool,
    runtime_exempt_prefix: Option<&str>,
    candidates: &[String],
    runner: &dyn JavaRunner,
) -> Vec<Java> {
    let hits = collect_search_hits(roots, include_subdirs, runtime_exempt_prefix);
    if hits.is_empty() {
        return Vec::new();
    }
    let outcome = filter_search_hits(hits);
    let mut javas: Vec<Java> = outcome.folders.iter().map(|f| Java::new(f)).collect();
    let mut checked = check_all(&mut javas, runner);
    sort_javas(&mut checked, candidates);
    checked
}

// ---------------------------------------------------------------------------
// GetJavaRequirement — ModJava.vb's constraint engine.
// ---------------------------------------------------------------------------

/// A calendar date as a (year, month, day) tuple; lexicographic order
/// is chronological order.
pub type SimpleDate = (i32, u32, u32);

#[derive(Debug, Clone)]
pub struct JavaRequirementInput {
    /// `VersionArgumentJavaV2` setting; 1 means a forced range string.
    pub setting_mode: i32,
    /// `VersionArgumentJavaRange` setting (used when mode is 1).
    pub setting_range: Option<String>,
    /// `McVersion.Valid` — the vanilla version was recognized.
    pub valid: bool,
    /// The comparable vanilla version (see `version.rs`), as the
    /// 3-part .NET Version PCL derives from it.
    pub vanilla: JavaVersion,
    pub release_time: SimpleDate,
    /// `javaVersion.majorVersion` (or HMCL's `java_version`) from the
    /// version JSON, when present.
    pub json_java_major: Option<i64>,
    /// `javaVersion.component` (or `java_component`), when present.
    pub json_java_component: Option<String>,
    pub has_optifine: bool,
    pub mod_loaders: ModLoaders,
    /// Loader versions by loader; a missing entry behaves as
    /// "未知版本", matching how `McVersion` fills its dictionary.
    pub forge_version: Option<String>,
    pub neoforge_version: Option<String>,
    pub fabric_version: Option<String>,
    /// The current login type is 统一通行证 (Nide).
    pub login_is_nide: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaRequirement {
    pub range: ValueRange,
    pub recommended_component: Option<String>,
    /// Constraint-conflict warnings, in the order the original logs
    /// them (each entry is the constraint that clashed).
    pub conflict_warnings: Vec<ValueRange>,
}

/// Faithful port of `ModJava.GetJavaRequirement`. Settings access and
/// the instance object are replaced by `JavaRequirementInput`.
pub fn java_requirement(input: &JavaRequirementInput) -> JavaRequirement {
    // A forced range in the version settings wins outright.
    if input.setting_mode == 1 {
        if let Some(raw) = &input.setting_range {
            if let Ok(range) = ValueRange::from_string(raw) {
                return JavaRequirement { range, recommended_component: None, conflict_warnings: Vec::new() };
            }
        }
        // Parse failure: the original logs a warning and falls through.
    }

    let mut range = ValueRange::all();
    let mut conflict_warnings: Vec<ValueRange> = Vec::new();
    let add_constraint = |range: &mut ValueRange, warnings: &mut Vec<ValueRange>, constraint: ValueRange| {
        let intersected = range.intersect(&constraint);
        if intersected.is_empty() {
            warnings.push(constraint);
            *range = constraint;
        } else {
            *range = intersected;
        }
    };

    let vanilla = input.vanilla;
    let valid = input.valid;
    let release = input.release_time;

    // Vanilla requirements, by version number or release date.
    if (!valid && release >= (2024, 4, 2)) || (valid && vanilla >= JavaVersion::dotnet3(20, 0, 5)) {
        add_constraint(&mut range, &mut conflict_warnings, ValueRange::at_least(JavaVersion::dotnet2(21, 0)));
    } else if (!valid && release >= (2021, 11, 16)) || (valid && vanilla.major >= 18) {
        add_constraint(&mut range, &mut conflict_warnings, ValueRange::at_least(JavaVersion::dotnet2(17, 0)));
    } else if (!valid && release >= (2021, 5, 11)) || (valid && vanilla.major >= 17) {
        add_constraint(&mut range, &mut conflict_warnings, ValueRange::at_least(JavaVersion::dotnet2(16, 0)));
    } else if release.0 >= 2017 {
        add_constraint(&mut range, &mut conflict_warnings, ValueRange::at_least(JavaVersion::dotnet2(8, 0)));
    } else if release <= (2013, 5, 1) && release.0 >= 2001 {
        add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(9, 0)));
    }

    // Mojang's own requirement from the version JSON (26+ era).
    let mut recommended_component: Option<String> = None;
    let recommended_code = input.json_java_major.unwrap_or(0);
    if recommended_code >= 22 {
        add_constraint(
            &mut range,
            &mut conflict_warnings,
            ValueRange::at_least(JavaVersion::dotnet2(recommended_code, 0)),
        );
        recommended_component = input
            .json_java_component
            .clone()
            .filter(|s| !s.is_empty());
    }

    // OptiFine (standard versions only).
    if input.has_optifine && valid {
        if vanilla.major < 7 {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(9, 0)));
        } else if vanilla.major >= 8 && vanilla.major < 12 {
            add_constraint(
                &mut range,
                &mut conflict_warnings,
                ValueRange::closed_open(JavaVersion::dotnet2(8, 0), JavaVersion::dotnet2(9, 0)),
            );
        } else if vanilla.major == 12 {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(9, 0)));
        }
    }

    // Forge (test results referenced as #8432 in the original).
    if input.mod_loaders.has(ModLoaders::FORGE) {
        let fv = input.forge_version.as_deref().unwrap_or("未知版本");
        if vanilla >= JavaVersion::dotnet3(6, 0, 1) && vanilla <= JavaVersion::dotnet3(7, 0, 2) {
            add_constraint(
                &mut range,
                &mut conflict_warnings,
                ValueRange::closed_open(JavaVersion::dotnet2(7, 0), JavaVersion::dotnet2(8, 0)),
            );
        } else if vanilla.major <= 12 || !valid {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(9, 0)));
        } else if vanilla.major <= 14 {
            add_constraint(
                &mut range,
                &mut conflict_warnings,
                ValueRange::closed_open(JavaVersion::dotnet2(8, 0), JavaVersion::dotnet2(11, 0)),
            );
        } else if vanilla.major == 15 {
            add_constraint(
                &mut range,
                &mut conflict_warnings,
                ValueRange::closed_open(JavaVersion::dotnet2(8, 0), JavaVersion::dotnet2(16, 0)),
            );
        } else if compare_version_ge(fv, "34.0.0") && compare_version_ge("36.2.25", fv) {
            add_constraint(
                &mut range,
                &mut conflict_warnings,
                ValueRange::at_most(JavaVersion::dotnet3(8, 0, 320)),
            );
        } else if compare_version_ge(fv, "36.2.26") && compare_version_ge("36.999999.999999", fv) {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(24, 0)));
        } else if compare_version_ge(fv, "37.0.0") && compare_version_ge("37.0.79", fv) {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(17, 0)));
        } else if vanilla.major == 18 && input.has_optifine {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(19, 0)));
        } else if compare_version_ge(fv, "45.0.21") && compare_version_ge("45.0.65", fv) {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(20, 0)));
        } else if compare_version_ge(fv, "45.0.66") && compare_version_ge("47.4.8", fv) {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(22, 0)));
        }
    }

    // NeoForge.
    if input.mod_loaders.has(ModLoaders::NEOFORGE) {
        let nv = input.neoforge_version.as_deref().unwrap_or("未知版本");
        if vanilla == JavaVersion::dotnet3(20, 0, 1)
            || (compare_version_ge("20.2.62-beta", nv) && !nv.contains("25w14craftmine"))
        {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(22, 0)));
        }
    }

    // Fabric (standard versions only).
    if input.mod_loaders.has(ModLoaders::FABRIC) && valid {
        if vanilla.major >= 15 && vanilla.major <= 16 {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::at_least(JavaVersion::dotnet2(8, 0)));
        } else if vanilla.major >= 18 {
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::at_least(JavaVersion::dotnet2(17, 0)));
        }
        let fv = input.fabric_version.as_deref().unwrap_or("未知版本");
        if fv != "未知版本" && compare_version(fv, "0.17.0") < 0 {
            // Fabric Loader 0.16.x and earlier (Mixin/ASM) break on Java 25.
            add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(25, 0)));
        }
    }

    // LiteLoader (standard versions only): at most Java 8.
    if input.mod_loaders.has(ModLoaders::LITELOADER) && valid {
        add_constraint(&mut range, &mut conflict_warnings, ValueRange::less_than(JavaVersion::dotnet2(9, 0)));
    }

    // 统一通行证 (Nide): comment in the original says 8u101, the code
    // requires 8.0.141 — the code wins, as in the original.
    if input.login_is_nide {
        add_constraint(
            &mut range,
            &mut conflict_warnings,
            ValueRange::at_least(JavaVersion::dotnet3(8, 0, 141)),
        );
    }

    JavaRequirement { range, recommended_component, conflict_warnings }
}

/// Select the first Java in list order that checks out and whose
/// version lies in `range` — the inner selection loop of
/// `SelectOrDownloadJava`'s automatic branch. (Re-searching and
/// auto-downloading on a miss are loader concerns, left to callers.)
pub fn select_from_list(
    javas: &mut [Java],
    range: &ValueRange,
    runner: &dyn JavaRunner,
) -> Option<Java> {
    for java in javas.iter_mut() {
        if !java.check(runner) {
            continue;
        }
        match java.version {
            Some(v) if range.contains(v) => return Some(java.clone()),
            _ => continue,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Mojang java-runtime list parsing & the manual-install prompt text
// (ModJava.vb, inside SelectOrDownloadJava).
// ---------------------------------------------------------------------------

/// Parse a Mojang runtime `version.name` ("12", "8u51-cacert462b08",
/// "17.0.8.1", "21.0.7" …) into a 4-part version, exactly as the
/// original's inline parsing does.
pub fn parse_mojang_runtime_version(name: &str) -> JavaVersion {
    if name.contains('.') {
        let parts: Vec<&str> = name.split('.').collect();
        let get = |i: usize| {
            if i < parts.len() {
                crate::vrange::vb_val_int(parts[i])
            } else {
                0
            }
        };
        JavaVersion::dotnet4(get(0), get(1), get(2), get(3))
    } else if name.starts_with("8u51-") {
        JavaVersion::dotnet4(8, 0, 51, 0)
    } else {
        let head: String = name.chars().take_while(|c| *c != 'u' && *c != '.').collect();
        JavaVersion::dotnet4(crate::vrange::vb_val_int(&head), 0, 0, 0)
    }
}

/// The `Description` fragment of the "请手动安装 Java" prompt, e.g.
/// ` Java 21` or ` Java 17 或更高版本的 Java` (leading space included,
/// as in the original). A range with neither bound crashes the
/// original; the port falls back to the raw range text.
pub fn describe_required_java(range: &ValueRange) -> String {
    let has_pure_lower = range.lower.is_some()
        && range.is_lower_inclusive
        && range.lower.unwrap().minor <= 0
        && range.lower.unwrap().build <= 0
        && range.lower.unwrap().revision <= 0;
    let has_pure_upper = range.upper.is_some()
        && !range.is_upper_inclusive
        && range.upper.unwrap().minor <= 0
        && range.upper.unwrap().build <= 0
        && range.upper.unwrap().revision <= 0;
    if let (Some(lower), Some(upper)) = (range.lower, range.upper) {
        if has_pure_lower && has_pure_upper {
            if lower.major == upper.major - 1 {
                format!(" Java {}", lower.major)
            } else {
                format!(" Java {} ~ {}", lower.major, upper.major - 1)
            }
        } else {
            format!(" Java {range}")
        }
    } else if let Some(lower) = range.lower {
        if has_pure_lower {
            format!(" Java {} 或更高版本的 Java", lower.major)
        } else if range.is_lower_inclusive {
            format!(" Java {lower} 或更高版本的 Java")
        } else {
            format!("版本高于 {lower} 的 Java")
        }
    } else if let Some(upper) = range.upper {
        if has_pure_upper {
            format!(" Java {} 或更低版本的 Java", upper.major - 1)
        } else if range.is_upper_inclusive {
            format!(" Java {upper} 或更低版本的 Java")
        } else {
            format!("版本低于 {upper} 的 Java")
        }
    } else {
        format!(" Java {range}")
    }
}

/// Convenience index for tests and callers that keep per-user Java
/// lists keyed by folder.
pub type JavaList = HashMap<String, Java>;

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRunner {
        outputs: HashMap<String, String>,
    }
    impl JavaRunner for FakeRunner {
        fn run(&self, exe_path: &str, _args: &[&str], _timeout: Duration) -> io::Result<String> {
            self.outputs
                .get(exe_path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such exe"))
        }
    }

    fn runner_with(entries: &[(&str, &str)]) -> FakeRunner {
        FakeRunner {
            outputs: entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    const JAVA21_OUTPUT: &str = "openjdk version \"21.0.7\" 2025-04-15\nOpenJDK Runtime Environment (build 21.0.7+8)\nOpenJDK 64-Bit Server VM (build 21.0.7+8, mixed mode)\n    file.encoding = UTF-8\n    native.encoding = UTF-8\n";
    const JAVA8_OUTPUT: &str = "java version \"1.8.0_202\"\nJava(TM) SE Runtime Environment (build 1.8.0_202-b08)\nJava HotSpot(TM) 64-Bit Server VM (build 25.202-b08, mixed mode)\n    file.encoding = GBK\n";

    #[test]
    fn parse_openjdk_21_output() {
        let probe = parse_check_output(JAVA21_OUTPUT).unwrap();
        assert_eq!(probe.version, JavaVersion::dotnet4(21, 0, 7, 0));
        // The original lowercases the whole output first.
        assert_eq!(probe.file_encoding.as_deref(), Some("utf-8"));
        assert_eq!(probe.native_encoding.as_deref(), Some("utf-8"));
    }

    #[test]
    fn parse_java8_output_strips_leading_1() {
        let probe = parse_check_output(JAVA8_OUTPUT).unwrap();
        assert_eq!(probe.version, JavaVersion::dotnet4(8, 0, 202, 0));
        assert_eq!(probe.file_encoding.as_deref(), Some("gbk"));
        // No native.encoding in old output: falls back to file.encoding.
        assert_eq!(probe.native_encoding.as_deref(), Some("gbk"));
    }

    #[test]
    fn parse_rejects_32bit_and_broken() {
        let out32 = "java version \"17.0.8\"\nJava HotSpot(TM) Server VM\n";
        assert_eq!(parse_check_output(out32), Err(JavaCheckError::Not64Bit));
        let fatal = "java version \"17.0.8\"\nA fatal error has been detected\n64-Bit\n";
        assert_eq!(parse_check_output(fatal), Err(JavaCheckError::FatalError));
        assert_eq!(parse_check_output(""), Err(JavaCheckError::EmptyOutput));
        let nover = "some random output with 64-bit inside\n";
        assert_eq!(parse_check_output(nover), Err(JavaCheckError::VersionNotFound));
    }

    #[test]
    fn java_check_caches_and_equality() {
        let runner = runner_with(&[(r"C:\Java\jdk-21\bin\java.exe", JAVA21_OUTPUT)]);
        let mut java = Java::new(r"C:\Java\jdk-21\bin");
        assert_eq!(java.folder, r"C:\Java\jdk-21\bin\");
        assert!(java.check(&runner));
        assert_eq!(java.version, Some(JavaVersion::dotnet4(21, 0, 7, 0)));
        assert!(java.check(&runner)); // cached
        assert_eq!(java, Java::new("c:/java/JDK-21/bin/"));
        let mut bad = Java::new(r"C:\Nowhere\bin");
        assert!(!bad.check(&runner));
    }

    #[test]
    fn sort_prefers_candidates_then_closest_to_21() {
        let mut javas = vec![
            Java::with_version(r"C:\Other\jdk-21\bin", JavaVersion::dotnet2(21, 0)),
            Java::with_version(r"C:\Java\jdk-17\bin", JavaVersion::dotnet2(17, 0)),
            Java::with_version(r"C:\Java\jdk-25\bin", JavaVersion::dotnet2(25, 0)),
        ];
        sort_javas(&mut javas, &[r"C:\Java".to_string()]);
        let order: Vec<&str> = javas.iter().map(|j| j.folder.as_str()).collect();
        assert_eq!(
            order,
            vec![r"C:\Java\jdk-17\bin\", r"C:\Java\jdk-25\bin\", r"C:\Other\jdk-21\bin\"]
        );
    }

    #[test]
    fn candidate_folder_list_shape() {
        let env = JavaCandidateEnv {
            app_data: r"C:\Users\x\AppData\Roaming".into(),
            user_profile: r"C:\Users\x".into(),
            local_app_data: r"C:\Users\x\AppData\Local".into(),
            program_files: r"C:\Program Files".into(),
            program_files_x86: r"C:\Program Files (x86)".into(),
            my_documents: r"C:\Users\x\Documents".into(),
            jdk_home: None,
            java_home: Some(r#""D:\Java\jdk-21""#.into()),
            microsoft_jdk_dirs: vec![r"C:\Program Files\Microsoft\jdk-21.0.7".into()],
        };
        let folders = candidate_folders(&env);
        assert_eq!(folders[0], r"C:\Users\x\AppData\Roaming\.minecraft\runtime");
        assert!(folders.contains(&r"C:\Program Files\Java".to_string()));
        assert!(folders.contains(&r"C:\Program Files\Microsoft\jdk-21.0.7".to_string()));
        // JAVA_HOME entry is unquoted and included.
        assert!(folders.contains(&r"D:\Java\jdk-21".to_string()));
    }

    #[test]
    fn search_hit_filtering() {
        let hits = vec![
            JavaSearchHit { folder: r"C:\Java\a\".into(), has_reparse_point: false, is_special_path: false },
            JavaSearchHit { folder: r"C:\Link\b\".into(), has_reparse_point: true, is_special_path: false },
            JavaSearchHit { folder: r"C:\Windows\System32\c\".into(), has_reparse_point: false, is_special_path: true },
        ];
        let out = filter_search_hits(hits);
        assert_eq!(out.folders, vec![r"C:\Java\a\".to_string()]);
        assert_eq!(out.reparse_dropped.len(), 1);
        assert_eq!(out.special_dropped.len(), 1);
        // Everything flagged -> everything kept.
        let all = vec![JavaSearchHit { folder: r"C:\Link\b\".into(), has_reparse_point: true, is_special_path: true }];
        let out2 = filter_search_hits(all);
        assert!(out2.all_reparse_kept && out2.all_special_kept);
        assert_eq!(out2.folders.len(), 1);
        assert!(is_special_path(r"C:\Program Files\Java\java8path_target_123\bin\"));
        assert!(!is_special_path(r"C:\Java\jdk-21\bin\"));
    }

    fn base_input() -> JavaRequirementInput {
        JavaRequirementInput {
            setting_mode: 0,
            setting_range: None,
            valid: true,
            vanilla: JavaVersion::dotnet3(20, 0, 1),
            release_time: (2023, 6, 1),
            json_java_major: None,
            json_java_component: None,
            has_optifine: false,
            mod_loaders: ModLoaders::NONE,
            forge_version: None,
            neoforge_version: None,
            fabric_version: None,
            login_is_nide: false,
        }
    }

    #[test]
    fn requirement_vanilla_eras() {
        // 1.20.1 -> Java 17 era (major >= 18 check on comparable (20,0,1) is
        // about the shifted major; PCL compares Vanilla.Major which is 20).
        let req = java_requirement(&base_input());
        assert_eq!(req.range, ValueRange::at_least(JavaVersion::dotnet2(17, 0)));
        // 1.20.5+ -> Java 21.
        let mut input = base_input();
        input.vanilla = JavaVersion::dotnet3(20, 0, 5);
        assert_eq!(
            java_requirement(&input).range,
            ValueRange::at_least(JavaVersion::dotnet2(21, 0))
        );
        // Old release by date only: 2016 -> below the 2017 line and after
        // 2013-05-01, so no constraint from the vanilla branch.
        let mut input = base_input();
        input.valid = false;
        input.vanilla = JavaVersion::dotnet3(9999, 0, 0);
        input.release_time = (2016, 1, 1);
        assert_eq!(java_requirement(&input).range, ValueRange::all());
        // 2012 release -> at most Java 8.
        input.release_time = (2012, 1, 1);
        assert_eq!(
            java_requirement(&input).range,
            ValueRange::less_than(JavaVersion::dotnet2(9, 0))
        );
    }

    #[test]
    fn requirement_forge_and_conflicts() {
        // Forge on 1.16.5 with Forge 36.2.25 -> at most Java 8u320.
        let mut input = base_input();
        input.vanilla = JavaVersion::dotnet3(16, 0, 5);
        input.release_time = (2020, 11, 1);
        input.mod_loaders = ModLoaders::FORGE;
        input.forge_version = Some("36.2.25".into());
        let req = java_requirement(&input);
        assert_eq!(req.range.upper, Some(JavaVersion::dotnet3(8, 0, 320)));
        assert!(req.range.contains(JavaVersion::dotnet4(8, 0, 202, 0)));
        assert!(!req.range.contains(JavaVersion::dotnet4(8, 0, 321, 0)));
        // Fabric 0.16 on 1.20.1 -> Java 17+ and < 25.
        let mut input = base_input();
        input.mod_loaders = ModLoaders::FABRIC;
        input.fabric_version = Some("0.16.9".into());
        let req = java_requirement(&input);
        assert!(req.range.contains(JavaVersion::dotnet2(21, 0)));
        assert!(!req.range.contains(JavaVersion::dotnet2(25, 0)));
        // Forced range setting wins outright.
        let mut input = base_input();
        input.setting_mode = 1;
        input.setting_range = Some("[11.0, 17.0]".into());
        let req = java_requirement(&input);
        assert_eq!(req.range.lower, Some(JavaVersion::dotnet4(11, 0, 0, 0)));
        assert_eq!(req.range.upper, Some(JavaVersion::dotnet4(17, 0, 0, 0)));
    }

    #[test]
    fn mojang_runtime_name_parsing() {
        assert_eq!(parse_mojang_runtime_version("21.0.7"), JavaVersion::dotnet4(21, 0, 7, 0));
        assert_eq!(parse_mojang_runtime_version("17.0.8.1"), JavaVersion::dotnet4(17, 0, 8, 1));
        assert_eq!(parse_mojang_runtime_version("12"), JavaVersion::dotnet4(12, 0, 0, 0));
        assert_eq!(
            parse_mojang_runtime_version("8u51-cacert462b08"),
            JavaVersion::dotnet4(8, 0, 51, 0)
        );
        assert_eq!(parse_mojang_runtime_version("8u202"), JavaVersion::dotnet4(8, 0, 0, 0));
    }

    #[test]
    fn describe_range_text() {
        assert_eq!(
            describe_required_java(&ValueRange::at_least(JavaVersion::dotnet2(21, 0))),
            " Java 21 或更高版本的 Java"
        );
        assert_eq!(
            describe_required_java(&ValueRange::closed_open(
                JavaVersion::dotnet2(8, 0),
                JavaVersion::dotnet2(9, 0)
            )),
            " Java 8"
        );
        assert_eq!(
            describe_required_java(&ValueRange::less_than(JavaVersion::dotnet2(25, 0))),
            " Java 24 或更低版本的 Java"
        );
    }

    #[test]
    fn select_from_list_picks_first_matching() {
        let runner = runner_with(&[
            (r"C:\Java\jdk-8\bin\java.exe", JAVA8_OUTPUT),
            (r"C:\Java\jdk-21\bin\java.exe", JAVA21_OUTPUT),
        ]);
        let mut javas = vec![Java::new(r"C:\Java\jdk-8\bin"), Java::new(r"C:\Java\jdk-21\bin")];
        let picked = select_from_list(
            &mut javas,
            &ValueRange::at_least(JavaVersion::dotnet2(17, 0)),
            &runner,
        )
        .unwrap();
        assert_eq!(picked.folder, r"C:\Java\jdk-21\bin\");
    }
}
