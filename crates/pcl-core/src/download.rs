//! Download sources, file checking and the file download engine.
//!
//! Ports of:
//! - `MeloongCore/Shared/Utils/FileUtils.cs` `FileChecker` — size /
//!   hash / JSON validation of a local file.
//! - `ModDownload.vb` `DlSource*` — the official ↔ BMCLAPI mirror URL
//!   rewriting and ordering rules (the mirror is
//!   `bmclapi2.bangbang93.com`, PCL's one built-in mirror; no other
//!   mirror is invented here).
//! - `ModNet.vb` `NetFile` / `LoaderDownload` strategy — multi-source
//!   failover, ranged piece downloads for files ≥ 1 MiB, per-source
//!   failure marking, a final single-threaded no-resume retry pass,
//!   and `FileChecker` verification of the merged result. The
//!   original's thread scheduler (256 KiB minimum piece, global
//!   thread limit `ToolDownloadThread + 1`, default 63 + 1) is modeled
//!   at strategy level; its sub-256 KiB/s stall watchdog and speed
//!   limiter are not modeled (noted in PORT-PLAN §M2).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::http::{HttpClient, HttpError, HttpRequest};

/// The BMCLAPI mirror root (all `DlSource*` mirror URLs build on it).
pub const BMCLAPI_ROOT: &str = "https://bmclapi2.bangbang93.com";

// ---------------------------------------------------------------------------
// FileChecker (MeloongCore FileUtils.cs).
// ---------------------------------------------------------------------------

/// Port of MeloongCore's `FileChecker`: validates a local file's
/// size, hash and (optionally) JSON well-formedness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChecker {
    /// Exact size in bytes; -1 skips the check.
    pub actual_size: i64,
    /// Minimum size in bytes; -1 skips the check.
    pub min_size: i64,
    /// MD5 / SHA1 / SHA256 / SHA512 hex, chosen by string length
    /// (32 / 40 / 64 / 128), as in the original.
    pub hash: Option<String>,
    /// Require the file to parse as JSON.
    pub is_json: bool,
}

impl Default for FileChecker {
    fn default() -> Self {
        FileChecker {
            actual_size: -1,
            min_size: -1,
            hash: None,
            is_json: false,
        }
    }
}

impl FileChecker {
    pub fn with_hash(hash: impl Into<String>) -> FileChecker {
        FileChecker {
            hash: Some(hash.into()),
            ..Default::default()
        }
    }

    /// `FileChecker.Check`: None means the file is usable; Some is
    /// the original's error description (no trailing period).
    pub fn check(&self, path: &Path) -> Option<String> {
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Some(format!("文件不存在：{}", path.display())),
        };
        let file_size = metadata.len() as i64;
        if self.actual_size >= 0 && self.actual_size != file_size {
            return Some(size_error(
                path,
                format!("文件大小应为 {} B，实际为 {} B", self.actual_size, file_size),
                file_size,
            ));
        }
        if self.min_size >= 0 && self.min_size > file_size {
            return Some(size_error(
                path,
                format!("文件大小应至少为 {} B，实际为 {} B", self.min_size, file_size),
                file_size,
            ));
        }
        if let Some(hash) = &self.hash {
            if !hash.is_empty() {
                let hash = hash.to_lowercase();
                let computed = match hash.len() {
                    32 => compute_file_hash::<md5::Md5>(path),
                    40 => compute_file_hash::<sha1::Sha1>(path),
                    64 => compute_file_hash::<sha2::Sha256>(path),
                    128 => compute_file_hash::<sha2::Sha512>(path),
                    _ => {
                        return Some(format!(
                            "无法识别的 Hash 类型（长度为 {}）：{}",
                            hash.len(),
                            hash
                        ))
                    }
                };
                match computed {
                    Ok(actual) => {
                        if actual != hash {
                            let kind = match hash.len() {
                                32 => "MD5",
                                40 => "SHA1",
                                64 => "SHA256",
                                _ => "SHA512",
                            };
                            return Some(format!("文件 {kind} 应为 {hash}，实际为 {actual}"));
                        }
                    }
                    Err(e) => return Some(e),
                }
            }
        }
        if self.is_json {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    if content.is_empty() {
                        return Some("读取到的文件为空".to_string());
                    }
                    if let Err(e) = serde_json::from_str::<serde_json::Value>(&content) {
                        return Some(e.to_string());
                    }
                }
                Err(e) => return Some(e.to_string()),
            }
        }
        None
    }
}

/// The original appends the file's content to size errors when the
/// file is under 2000 bytes (it is usually an error page).
fn size_error(path: &Path, message: String, file_size: i64) -> String {
    if file_size < 2000 {
        if let Ok(content) = std::fs::read_to_string(path) {
            return format!("{message}，内容为：{content}");
        }
    }
    message
}

fn compute_file_hash<D: digest::Digest>(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = D::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Download source selection (ModDownload.vb, "DlSource | 镜像下载源").
// ---------------------------------------------------------------------------

/// `Settings("ToolDownloadSource")` / `ToolDownloadVersion` values:
/// 0 = mirror first, 1 = automatic (measured), 2 = official first.
/// In automatic mode the measured flag comes from timing the Mojang
/// manifest fetch (< 4000 ms → official is preferred), exactly as
/// `DlPreferMojang` is set in `DlClientListMojangMain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePreference {
    MirrorFirst,
    Auto { prefer_official_measured: bool },
    OfficialFirst,
}

impl SourcePreference {
    /// `DlSourcePreferMojang` / `DlVersionListPreferMojang`.
    pub fn prefer_official(&self) -> bool {
        match self {
            SourcePreference::MirrorFirst => false,
            SourcePreference::Auto {
                prefer_official_measured,
            } => *prefer_official_measured,
            SourcePreference::OfficialFirst => true,
        }
    }

    pub fn from_setting(value: i64, prefer_official_measured: bool) -> SourcePreference {
        match value {
            0 => SourcePreference::MirrorFirst,
            2 => SourcePreference::OfficialFirst,
            _ => SourcePreference::Auto {
                prefer_official_measured,
            },
        }
    }
}

/// `DlSourceOrder`: union of official and mirror URLs, ordered by
/// preference (duplicates removed, first occurrence wins — .NET
/// `Union` semantics).
pub fn dl_source_order(
    pref: SourcePreference,
    official: &[String],
    mirror: &[String],
) -> Vec<String> {
    let (first, second) = if pref.prefer_official() {
        (official, mirror)
    } else {
        (mirror, official)
    };
    let mut out: Vec<String> = Vec::new();
    for url in first.iter().chain(second.iter()) {
        if !out.contains(url) {
            out.push(url.clone());
        }
    }
    out
}

const BMCLAPI: &str = "https://bmclapi2.bangbang93.com";

/// `DlSourceAssetsGet`: asset object downloads.
pub fn dl_source_assets_get(pref: SourcePreference, original: &str) -> Vec<String> {
    let original = original.replace(
        "http://resources.download.minecraft.net",
        "https://resources.download.minecraft.net",
    );
    let mirror = original
        .replace("https://piston-data.mojang.com", &format!("{BMCLAPI}/assets"))
        .replace("https://piston-meta.mojang.com", &format!("{BMCLAPI}/assets"))
        .replace(
            "https://resources.download.minecraft.net",
            &format!("{BMCLAPI}/assets"),
        );
    dl_source_order(pref, &[original], &[mirror])
}

/// `DlSourceLibraryGet`: library downloads. Forge / Fabric /
/// NeoForged URLs never get the official source prepended (the
/// original comment: 不添加原版源) — only the two BMCLAPI variants.
pub fn dl_source_library_get(pref: SourcePreference, original: &str) -> Vec<String> {
    let maven = original
        .replace("https://piston-data.mojang.com", &format!("{BMCLAPI}/maven"))
        .replace("https://piston-meta.mojang.com", &format!("{BMCLAPI}/maven"))
        .replace("https://libraries.minecraft.net", &format!("{BMCLAPI}/maven"));
    let libraries = original
        .replace("https://piston-data.mojang.com", &format!("{BMCLAPI}/libraries"))
        .replace("https://piston-meta.mojang.com", &format!("{BMCLAPI}/libraries"))
        .replace(
            "https://libraries.minecraft.net",
            &format!("{BMCLAPI}/libraries"),
        );
    if ["minecraftforge", "fabricmc", "neoforged"]
        .iter()
        .any(|k| original.contains(k))
    {
        return vec![maven, libraries];
    }
    dl_source_order(pref, &[original.to_string()], &[maven, libraries])
}

/// `DlSourceLauncherOrMetaGet`: launcher / meta files (client jar,
/// version JSON, asset index). Errors on a missing URL, as the
/// original throws.
pub fn dl_source_launcher_or_meta_get(
    pref: SourcePreference,
    original: &str,
) -> Vec<String> {
    let mirror = original
        .replace("https://piston-data.mojang.com", BMCLAPI)
        .replace("https://piston-meta.mojang.com", BMCLAPI)
        .replace("https://launcher.mojang.com", BMCLAPI)
        .replace("https://launchermeta.mojang.com", BMCLAPI);
    dl_source_order(pref, &[original.to_string()], &[mirror])
}

/// `DlSourceModGet`: the MCIMirror rewrite for Mod downloads (not
/// used by the vanilla pipeline; ported for completeness).
pub fn dl_source_mod_get(original: &str) -> String {
    original
        .replace("api.modrinth.com", "mod.mcimirror.top/modrinth")
        .replace("staging-api.modrinth.com", "mod.mcimirror.top/modrinth")
        .replace("cdn.modrinth.com", "mod.mcimirror.top")
        .replace("api.curseforge.com", "mod.mcimirror.top/curseforge")
        .replace("edge.forgecdn.net", "mod.mcimirror.top")
        .replace("mediafilez.forgecdn.net", "mod.mcimirror.top")
        .replace("media.forgecdn.net", "mod.mcimirror.top")
}

// ---------------------------------------------------------------------------
// NetFile + download engine (ModNet.vb strategy model).
// ---------------------------------------------------------------------------

/// Port of `NetFile`: one local file plus its ordered download
/// sources and checker. Sources are deduplicated on construction,
/// as in the original.
#[derive(Debug, Clone)]
pub struct NetFile {
    pub sources: Vec<String>,
    pub local_path: PathBuf,
    pub checker: FileChecker,
}

impl NetFile {
    pub fn new(sources: Vec<String>, local_path: PathBuf, checker: FileChecker) -> NetFile {
        let mut deduped: Vec<String> = Vec::new();
        for source in sources {
            let source = source.replace(['\r', '\n'], "").trim().to_string();
            if !deduped.contains(&source) {
                deduped.push(source);
            }
        }
        NetFile {
            sources: deduped,
            local_path,
            checker,
        }
    }

    /// The file name portion of the local path (`LocalName`).
    pub fn local_name(&self) -> String {
        self.local_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Download engine configuration.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Global thread limit: upstream `ToolDownloadThread + 1`
    /// (setting default 63, so 64).
    pub thread_limit: usize,
    /// How many files `download_all` works on at once. Upstream
    /// interleaves files freely under the global thread limit; the
    /// port divides the limit across this many files.
    pub parallel_files: usize,
    /// Directory for piece temp files (upstream:
    /// `<temp>/Download/`). Pieces persist across runs keyed by
    /// target path + range, which extends the original's in-session
    /// piece reuse into断点续传 across runs.
    pub temp_dir: PathBuf,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        DownloadOptions {
            thread_limit: 64,
            parallel_files: 8,
            temp_dir: std::env::temp_dir().join("rerust-pcl-download"),
        }
    }
}

/// Outcome of ensuring one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// The local file already passed its checker.
    AlreadyOk,
    /// Downloaded (or repaired) and verified; `bytes` is the final
    /// file size, `source` the URL that produced the accepted file.
    Downloaded { bytes: u64, source: String },
}

/// Aggregated download failure (the original collects one exception
/// per failed source).
#[derive(Debug, Clone)]
pub struct DownloadError {
    pub local_path: PathBuf,
    pub source_errors: Vec<(String, String)>,
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "下载 {} 失败", self.local_path.display())?;
        for (source, error) in &self.source_errors {
            write!(f, "\n  [{source}] {error}")?;
        }
        Ok(())
    }
}
impl std::error::Error for DownloadError {}

/// Files below this size are never split (`NetFile.IsNoSplit`).
const NO_SPLIT_BELOW: u64 = 1024 * 1024;
/// Minimum piece size (`FilePieceLimit`).
const PIECE_MIN: u64 = 256 * 1024;

/// The download engine. Stateless apart from its transport +
/// options; safe to share across threads.
pub struct DownloadEngine<'a> {
    http: &'a dyn HttpClient,
    options: DownloadOptions,
}

impl<'a> DownloadEngine<'a> {
    pub fn new(http: &'a dyn HttpClient, options: DownloadOptions) -> DownloadEngine<'a> {
        DownloadEngine { http, options }
    }

    /// Ensure one file: check → download (multi-source, pieces) →
    /// single-threaded retry pass → verify.
    pub fn ensure_file(&self, file: &NetFile) -> Result<DownloadOutcome, DownloadError> {
        if file.checker.check(&file.local_path).is_none() {
            return Ok(DownloadOutcome::AlreadyOk);
        }
        let mut errors: Vec<(String, String)> = Vec::new();
        // Round A: each source in order, ranged/piece download.
        for source in &file.sources {
            match self.download_from(file, source, true) {
                Ok(bytes) => {
                    return Ok(DownloadOutcome::Downloaded {
                        bytes,
                        source: source.clone(),
                    })
                }
                Err(e) => errors.push((source.clone(), e)),
            }
        }
        // Round B (the original's `Retried` pass): every source once
        // more, single-threaded and without resume, which works
        // around sources whose files are subtly wrong under parallel
        // ranged requests.
        for source in &file.sources {
            match self.download_from(file, source, false) {
                Ok(bytes) => {
                    return Ok(DownloadOutcome::Downloaded {
                        bytes,
                        source: source.clone(),
                    })
                }
                Err(e) => errors.push((source.clone(), format!("单线程重试：{e}"))),
            }
        }
        Err(DownloadError {
            local_path: file.local_path.clone(),
            source_errors: errors,
        })
    }

    /// Download `file` from one `source`; on success the checker has
    /// passed and the final size is returned.
    fn download_from(
        &self,
        file: &NetFile,
        source: &str,
        allow_pieces: bool,
    ) -> Result<u64, String> {
        if let Some(parent) = file.local_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let total = self.probe_size(source).ok().flatten();
        let bytes = match total {
            Some(size) if allow_pieces && size >= NO_SPLIT_BELOW => {
                self.download_pieces(file, source, size)?
            }
            _ => self.download_whole(file, source)?,
        };
        if let Some(error) = file.checker.check(&file.local_path) {
            return Err(format!("文件校验失败：{error}"));
        }
        Ok(bytes)
    }

    /// Probe the total size + range support with a 1-byte ranged
    /// GET. A 206 answer's `Content-Range` discloses the total; a
    /// 200 answer means the server ignored the Range header, so
    /// the size stays unknown (the original's `FileSize = -1`
    /// case) and the file is fetched single-stream.
    fn probe_size(&self, source: &str) -> Result<Option<u64>, String> {
        let req = HttpRequest::get(source).header("Range", "bytes=0-0");
        let resp = self.http.request(&req).map_err(|e| match e {
            HttpError::Status { status, .. } if status == 416 => {
                "服务器不支持分片下载（416）".to_string()
            }
            other => other.to_string(),
        })?;
        if resp.status == 206 {
            if let Some(range) = resp.header("Content-Range") {
                // "bytes 0-0/<total>"
                if let Some(total) = range.rsplit('/').next() {
                    if let Ok(total) = total.trim().parse::<u64>() {
                        return Ok(Some(total));
                    }
                }
            }
            return Ok(None);
        }
        Ok(None)
    }

    /// Single-stream whole-file download via a temp file + rename.
    fn download_whole(&self, file: &NetFile, source: &str) -> Result<u64, String> {
        let resp = self
            .http
            .request(&HttpRequest::get(source))
            .map_err(|e| e.to_string())?;
        let temp = self.temp_path_for(file, "whole");
        std::fs::create_dir_all(&self.options.temp_dir).map_err(|e| e.to_string())?;
        std::fs::write(&temp, &resp.body).map_err(|e| e.to_string())?;
        let bytes = resp.body.len() as u64;
        std::fs::rename(&temp, &file.local_path).map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    /// Ranged piece download: split `size` into pieces, fetch them
    /// in parallel from `source`, merge in order into the target.
    fn download_pieces(&self, file: &NetFile, source: &str, size: u64) -> Result<u64, String> {
        let threads = self
            .options
            .thread_limit
            .max(1)
            .min((size / PIECE_MIN) as usize + 1);
        let piece_size = (size / (threads as u64 * 4)).max(PIECE_MIN);
        let piece_count = size.div_ceil(piece_size);
        std::fs::create_dir_all(&self.options.temp_dir).map_err(|e| e.to_string())?;

        let next = std::sync::atomic::AtomicU64::new(0);
        let failure: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        let workers = (threads as u64).min(piece_count).max(1);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if index >= piece_count {
                        return;
                    }
                    let start = index * piece_size;
                    let end = ((index + 1) * piece_size).min(size) - 1;
                    if let Err(e) = self.fetch_piece(file, source, index, start, end) {
                        let mut slot = failure.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                        return;
                    }
                });
            }
        });
        if let Some(error) = failure.into_inner().unwrap() {
            return Err(error);
        }
        // Merge pieces in order.
        let temp = self.temp_path_for(file, "merged");
        {
            let mut out = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
            use std::io::Write;
            for index in 0..piece_count {
                let piece = self.piece_path(file, index);
                let data = std::fs::read(&piece).map_err(|e| e.to_string())?;
                out.write_all(&data).map_err(|e| e.to_string())?;
                let _ = std::fs::remove_file(&piece);
            }
        }
        std::fs::rename(&temp, &file.local_path).map_err(|e| e.to_string())?;
        Ok(size)
    }

    /// Fetch one byte range into its piece file; an existing piece
    /// file with the exact expected length is reused (resume).
    fn fetch_piece(
        &self,
        file: &NetFile,
        source: &str,
        index: u64,
        start: u64,
        end: u64,
    ) -> Result<(), String> {
        let piece = self.piece_path(file, index);
        let expected = end - start + 1;
        if let Ok(metadata) = std::fs::metadata(&piece) {
            if metadata.len() == expected {
                return Ok(());
            }
        }
        let req = HttpRequest::get(source).header("Range", &format!("bytes={start}-{end}"));
        let resp = self.http.request(&req).map_err(|e| e.to_string())?;
        if resp.body.len() as u64 != expected {
            return Err(format!(
                "分片 {index} 大小不符：应为 {expected} B，实际为 {} B",
                resp.body.len()
            ));
        }
        std::fs::write(&piece, &resp.body).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn piece_path(&self, file: &NetFile, index: u64) -> PathBuf {
        self.options
            .temp_dir
            .join(format!("{}-{index}.piece", file_key(file)))
    }

    fn temp_path_for(&self, file: &NetFile, kind: &str) -> PathBuf {
        self.options
            .temp_dir
            .join(format!("{}-{kind}.tmp", file_key(file)))
    }
}

/// Stable per-target key for temp file names (upstream names pieces
/// `{fileUuid}_{threadUuid}_{random}.tmp`; the port keys by target
/// path so pieces survive across runs for resume).
fn file_key(file: &NetFile) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(file.local_path.to_string_lossy().as_bytes());
    hex_lower(&hasher.finalize())
}

/// Download many files, `parallel_files` at a time. Returns one
/// result per input file, in input order. `progress` is called
/// after each file finishes (index, total, outcome summary).
pub fn download_all(
    engine: &DownloadEngine,
    files: &[NetFile],
    progress: &(dyn Fn(usize, usize, &NetFile, &Result<DownloadOutcome, DownloadError>) + Sync),
) -> Vec<Result<DownloadOutcome, DownloadError>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let results: Vec<std::sync::Mutex<Option<Result<DownloadOutcome, DownloadError>>>> =
        files.iter().map(|_| std::sync::Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let workers = engine.options.parallel_files.max(1).min(files.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::SeqCst);
                if index >= files.len() {
                    return;
                }
                let result = engine.ensure_file(&files[index]);
                progress(
                    done.fetch_add(1, Ordering::SeqCst) + 1,
                    files.len(),
                    &files[index],
                    &result,
                );
                *results[index].lock().unwrap() = Some(result);
            });
        }
    });
    results
        .into_iter()
        .map(|slot| slot.into_inner().unwrap().unwrap())
        .collect()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestClient;
    use std::io::{Read as IoRead, Write as IoWrite};

    #[test]
    fn source_rewriting_rules() {
        // Assets: official host → BMCLAPI /assets, order by pref.
        let urls = dl_source_assets_get(
            SourcePreference::MirrorFirst,
            "https://resources.download.minecraft.net/ab/cdef",
        );
        assert_eq!(
            urls,
            vec![
                "https://bmclapi2.bangbang93.com/assets/ab/cdef".to_string(),
                "https://resources.download.minecraft.net/ab/cdef".to_string(),
            ]
        );
        // Plain library: official + /maven + /libraries variants.
        let urls = dl_source_library_get(
            SourcePreference::OfficialFirst,
            "https://libraries.minecraft.net/com/google/guava/guava.jar",
        );
        assert_eq!(
            urls,
            vec![
                "https://libraries.minecraft.net/com/google/guava/guava.jar".to_string(),
                "https://bmclapi2.bangbang93.com/maven/com/google/guava/guava.jar".to_string(),
                "https://bmclapi2.bangbang93.com/libraries/com/google/guava/guava.jar".to_string(),
            ]
        );
        // Fabric/Forge/NeoForged (as named entries under
        // libraries.minecraft.net): no official source, ever.
        let urls = dl_source_library_get(
            SourcePreference::OfficialFirst,
            "https://libraries.minecraft.net/net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar",
        );
        assert_eq!(urls.len(), 2);
        assert!(urls.iter().all(|u| u.contains("bmclapi2.bangbang93.com")));
        assert!(urls[0].contains("/maven/"));
        // Launcher/meta: host root swapped to the mirror root.
        let urls = dl_source_launcher_or_meta_get(
            SourcePreference::MirrorFirst,
            "https://piston-meta.mojang.com/v1/packages/x/1.20.1.json",
        );
        assert_eq!(
            urls,
            vec![
                "https://bmclapi2.bangbang93.com/v1/packages/x/1.20.1.json".to_string(),
                "https://piston-meta.mojang.com/v1/packages/x/1.20.1.json".to_string(),
            ]
        );
        assert_eq!(
            SourcePreference::from_setting(0, true),
            SourcePreference::MirrorFirst
        );
        assert_eq!(
            SourcePreference::from_setting(2, false),
            SourcePreference::OfficialFirst
        );
        assert!(!SourcePreference::from_setting(1, false).prefer_official());
        assert!(SourcePreference::from_setting(1, true).prefer_official());
    }

    #[test]
    fn net_file_dedupes_sources() {
        let file = NetFile::new(
            vec![
                "https://a/x\r\n".to_string(),
                "https://a/x".to_string(),
                " https://b/y ".to_string(),
            ],
            PathBuf::from("f.bin"),
            FileChecker::default(),
        );
        assert_eq!(file.sources, vec!["https://a/x", "https://b/y"]);
    }

    /// A minimal HTTP/1.1 file server for engine tests: honors
    /// Range when `honor_range` is set, always closes connections.
    struct TestServer {
        url: String,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn start(content: Vec<u8>, honor_range: bool) -> TestServer {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_thread = stop.clone();
            let handle = std::thread::spawn(move || {
                while !stop_thread.load(std::sync::atomic::Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                            let mut buf: Vec<u8> = Vec::new();
                            let mut chunk = [0u8; 4096];
                            loop {
                                match stream.read(&mut chunk) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        buf.extend_from_slice(&chunk[..n]);
                                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let request = String::from_utf8_lossy(&buf).into_owned();
                            let range = request
                                .lines()
                                .find(|l| l.to_lowercase().starts_with("range:"))
                                .and_then(|l| l.split('=').nth(1))
                                .and_then(|r| {
                                    let (a, b) = r.trim().split_once('-')?;
                                    Some((
                                        a.parse::<usize>().ok()?,
                                        b.parse::<usize>().ok()?,
                                    ))
                                });
                            let (status, headers, body): (&str, String, Vec<u8>) =
                                match (honor_range, range) {
                                    (true, Some((start, end))) => {
                                        let end = end.min(content.len().saturating_sub(1));
                                        let body = content[start..=end].to_vec();
                                        (
                                            "206 Partial Content",
                                            format!(
                                                "Content-Range: bytes {start}-{end}/{}\r\nContent-Length: {}",
                                                content.len(),
                                                body.len()
                                            ),
                                            body,
                                        )
                                    }
                                    _ => (
                                        "200 OK",
                                        format!("Content-Length: {}", content.len()),
                                        content.clone(),
                                    ),
                                };
                            let response = format!(
                                "HTTP/1.1 {status}\r\n{headers}\r\nConnection: close\r\n\r\n"
                            );
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.write_all(&body);
                            let _ = stream.flush();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            TestServer {
                url: format!("http://127.0.0.1:{port}/file.bin"),
                stop,
                handle: Some(handle),
            }
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rerust-dl-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha1_hex(data: &[u8]) -> String {
        use sha1::Digest;
        let mut hasher = sha1::Sha1::new();
        hasher.update(data);
        hex_lower(&hasher.finalize())
    }

    #[test]
    fn engine_whole_download_and_already_ok() {
        let content: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let server = TestServer::start(content.clone(), false);
        let dir = test_dir("whole");
        let target = dir.join("f.bin");
        let file = NetFile::new(
            vec![server.url.clone()],
            target.clone(),
            FileChecker::with_hash(sha1_hex(&content)),
        );
        let http = ReqwestClient::new();
        let engine = DownloadEngine::new(
            &http,
            DownloadOptions {
                temp_dir: dir.join("tmp"),
                ..DownloadOptions::default()
            },
        );
        match engine.ensure_file(&file).unwrap() {
            DownloadOutcome::Downloaded { bytes, .. } => {
                assert_eq!(bytes, content.len() as u64)
            }
            other => panic!("expected Downloaded, got {other:?}"),
        }
        assert_eq!(std::fs::read(&target).unwrap(), content);
        // Second pass: the checker passes, no download happens.
        assert_eq!(engine.ensure_file(&file).unwrap(), DownloadOutcome::AlreadyOk);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_pieces_download() {
        // 2 MiB patterned content forces the ranged path.
        let content: Vec<u8> = (0..(2 * 1024 * 1024u32)).map(|i| ((i / 7) % 253) as u8).collect();
        let server = TestServer::start(content.clone(), true);
        let dir = test_dir("pieces");
        let target = dir.join("big.bin");
        let file = NetFile::new(
            vec![server.url.clone()],
            target.clone(),
            FileChecker::with_hash(sha1_hex(&content)),
        );
        let http = ReqwestClient::new();
        let engine = DownloadEngine::new(
            &http,
            DownloadOptions {
                temp_dir: dir.join("tmp"),
                ..DownloadOptions::default()
            },
        );
        engine.ensure_file(&file).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_fails_over_and_reports_check_failures() {
        let content = b"the real file".to_vec();
        let server = TestServer::start(content.clone(), false);
        let dir = test_dir("failover");
        let target = dir.join("f.bin");
        // Dead source first, good source second.
        let file = NetFile::new(
            vec![
                "http://127.0.0.1:1/dead.bin".to_string(),
                server.url.clone(),
            ],
            target.clone(),
            FileChecker::with_hash(sha1_hex(&content)),
        );
        let http = ReqwestClient::new();
        let engine = DownloadEngine::new(
            &http,
            DownloadOptions {
                temp_dir: dir.join("tmp"),
                ..DownloadOptions::default()
            },
        );
        engine.ensure_file(&file).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), content);
        // Wrong content for the checker → error mentioning 校验.
        let bad = NetFile::new(
            vec![server.url.clone()],
            dir.join("bad.bin"),
            FileChecker::with_hash(sha1_hex(b"something else")),
        );
        let err = engine.ensure_file(&bad).unwrap_err();
        assert!(err.to_string().contains("校验"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
