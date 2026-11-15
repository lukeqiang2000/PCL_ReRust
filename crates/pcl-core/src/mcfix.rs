//! "Fix" (complete-the-files) planning: turns a resolved instance
//! into the [`NetFile`] download list.
//!
//! Ports of `ModDownload.vb`'s `DlClientFix` and its helpers:
//! - the client jar (`DlClientJarGet`: the *base* instance of the
//!   inheritance chain, checker `MinSize = 1024` + exact size +
//!   SHA1, sources via `DlSourceLauncherOrMetaGet`),
//! - the libraries (`DlClientListLoader` /
//!   `McLibNetFilesFromTokens`: SHA1+size checkers except LabyMod,
//!   BMCLAPI maven variants, OptiFine special source),
//! - the asset index (`DlClientAssetIndexGet`, `IsJson` check only,
//!   legacy fallback index when the JSON has none),
//! - the asset objects (`McAssetsListGet`: objects →
//!   `assets/objects/<hh>/<hash>`, `virtual` →
//!   `assets/virtual/legacy`, `map_to_resources` → the instance's
//!   `resources` folder).
//!
//! Path note: M1's library model works on PCL-style strings
//! (backslash-joined under an `mc_folder` like `C:\MC\`). The
//! conversion helpers below translate between those and native
//! paths; on Windows they are the identity.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::download::{
    dl_source_assets_get, dl_source_launcher_or_meta_get, dl_source_library_get, FileChecker,
    NetFile, SourcePreference, BMCLAPI_ROOT,
};
use crate::instance::{token_string, LibEntry};

/// Convert a native path to the PCL string form (`\`-joined). On
/// Windows this is the identity (native paths already use `\`).
pub fn to_pcl_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

/// Convert a PCL string path back to a native path.
///
/// The PCL form is a Windows-shaped string (`\`-joined, possibly
/// drive- or UNC-rooted). The conversion is done at the string
/// level so the root survives: a drive-absolute path stays
/// drive-absolute (`C:\a\b` — a segment-by-segment `PathBuf::push`
/// rebuild used to drop the root and produce the drive-relative
/// `C:a\b`, which broke every classpath entry on the M2
/// real-machine run), a UNC path keeps its `\\server\share`
/// prefix, a rooted path keeps its root, and a drive-relative
/// `C:foo` stays drive-relative, exactly as written. Components
/// are re-joined with the host's `MAIN_SEPARATOR`, so on Windows
/// the result is the original string modulo duplicate
/// separators, and the `to_pcl_path` round trip is exact for
/// well-formed paths.
pub fn to_native_path(pcl: &str) -> PathBuf {
    let sep = std::path::MAIN_SEPARATOR;
    let bytes = pcl.as_bytes();
    let mut prefix = String::new();
    let mut rest = pcl;
    if pcl.starts_with("\\\\") || pcl.starts_with("//") {
        // UNC: the server and share belong to the root, not to
        // the component list.
        let mut parts = pcl[2..].splitn(3, ['\\', '/']);
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        prefix = format!("{sep}{sep}{server}");
        if !share.is_empty() {
            prefix = format!("{prefix}{sep}{share}");
        }
        rest = parts.next().unwrap_or_default();
        if !rest.is_empty() {
            prefix.push(sep);
        }
    } else if bytes.len() > 1 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        // Windows drive: `C:` plus the root separator when the
        // path is drive-absolute (`C:\...`); a bare `C:foo` stays
        // drive-relative, exactly as written.
        prefix.push_str(&pcl[..2]);
        rest = &pcl[2..];
        if rest.starts_with(['\\', '/']) {
            prefix.push(sep);
            rest = rest.trim_start_matches(['\\', '/']);
        }
    } else if pcl.starts_with(['\\', '/']) {
        prefix.push(sep);
        rest = pcl.trim_start_matches(['\\', '/']);
    }
    let mut out = prefix;
    for (index, part) in rest
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if index > 0 {
            out.push(sep);
        }
        out.push_str(part);
    }
    PathBuf::from(out)
}

/// mc_folder in PCL form with the trailing separator M1 expects
/// (e.g. `C:\MC\`).
pub fn mc_folder_pcl(mc_folder: &Path) -> String {
    let mut s = to_pcl_path(mc_folder);
    if !s.ends_with('\\') {
        s.push('\\');
    }
    s
}

/// The primary (client) jar location for an instance name:
/// `versions/<name>/<name>.jar`.
pub fn primary_jar_path(mc_folder: &Path, name: &str) -> PathBuf {
    mc_folder
        .join("versions")
        .join(name)
        .join(format!("{name}.jar"))
}

/// `DlClientJarGet` for the base instance's resolved JSON: a
/// NetFile for `downloads.client`, or None when the JSON carries no
/// client download (upstream throws and the caller logs a warning).
pub fn client_jar_net_file(
    base_resolved_json: &Value,
    base_name: &str,
    mc_folder: &Path,
    pref: SourcePreference,
) -> Option<NetFile> {
    let client = base_resolved_json.get("downloads")?.get("client")?;
    let url = token_string(client.get("url"))?;
    let size = client.get("size").and_then(Value::as_i64).unwrap_or(-1);
    let checker = FileChecker {
        min_size: 1024,
        actual_size: size,
        hash: token_string(client.get("sha1")),
        is_json: false,
    };
    Some(NetFile {
        local_path: primary_jar_path(mc_folder, base_name),
        sources: dl_source_launcher_or_meta_get(pref, &url),
        checker,
    })
}

/// `McLibNetFilesFromTokens`: download plan for the library list.
pub fn library_net_files(
    entries: &[LibEntry],
    mc_folder_pcl_str: &str,
    pref: SourcePreference,
) -> Vec<NetFile> {
    let libraries_prefix = format!("{mc_folder_pcl_str}libraries\\");
    let mut result: Vec<NetFile> = Vec::new();
    for entry in entries {
        // The main-jar pseudo entry is handled by the jar download.
        if entry.original_name.as_deref() == Some("client") {
            continue;
        }
        // transformer-discovery-service ships inside PCL's resources
        // upstream and is released at launch, not downloaded.
        if entry.local_path.contains("transformer-discovery-service") {
            continue;
        }
        let checker = if entry
            .original_name
            .as_deref()
            .map(|n| n.to_lowercase().contains("labymod"))
            .unwrap_or(false)
        {
            FileChecker::default()
        } else {
            FileChecker {
                min_size: -1,
                actual_size: if entry.size == 0 { -1 } else { entry.size },
                hash: entry.sha1.clone(),
                is_json: false,
            }
        };
        let mut urls: Vec<String> = Vec::new();
        // Forge "universal" artifacts whose JSON omits the URL get
        // one synthesized from maven.minecraftforge.net (#5455).
        let entry_url = entry.url.clone().or_else(|| {
            if entry.original_name.as_deref() == Some("net.minecraftforge:forge:universal") {
                let rel = entry
                    .local_path
                    .replace(&format!("{mc_folder_pcl_str}libraries"), "")
                    .replace('\\', "/");
                Some(format!("https://maven.minecraftforge.net{rel}"))
            } else {
                None
            }
        });
        if let Some(url) = &entry_url {
            urls.push(url.clone());
            if url.contains("launcher.mojang.com/v1/objects")
                || url.contains("client.txt")
                || url.contains(".tsrg")
            {
                urls.extend(dl_source_launcher_or_meta_get(pref, url));
            }
            if url.contains("maven") {
                let variant = maven_mirror_variant(url);
                if pref.prefer_official() {
                    urls.push(variant);
                } else {
                    urls.insert(0, variant);
                }
            }
        }
        if entry.local_path.contains("optifine\\OptiFine") {
            // OptiFine main jar: BMCLAPI's com/optifine tree, with
            // the path reshaping upstream performs (version dir up
            // to the first "_", file name dashes → underscores,
            // "_pre" builds under preview_).
            let prefix = format!("{mc_folder_pcl_str}libraries\\optifine\\OptiFine\\");
            let rest = entry.local_path.replace(&prefix, "");
            let version_part = rest.split('_').next().unwrap_or_default();
            let file_part = entry
                .local_path
                .rsplit('\\')
                .next()
                .unwrap_or_default()
                .replace('-', "_");
            let mut base = format!("/maven/com/optifine/{version_part}/{file_part}");
            if base.contains("_pre") {
                base = base.replace("com/optifine/", "com/optifine/preview_");
            }
            urls.push(format!("{BMCLAPI_ROOT}{base}"));
        } else if urls.len() <= 2 {
            if let Some(rel) = entry.local_path.strip_prefix(&libraries_prefix) {
                let rel = rel.replace('\\', "/");
                urls.extend(dl_source_library_get(
                    pref,
                    &format!("https://libraries.minecraft.net/{rel}"),
                ));
            }
        }
        if urls.is_empty() {
            continue;
        }
        let net_file = NetFile {
            local_path: to_native_path(&entry.local_path),
            sources: urls,
            checker,
        };
        // DistinctBy local path, first wins (upstream).
        if !result.iter().any(|f| f.local_path == net_file.local_path) {
            result.push(net_file);
        }
    }
    result
}

/// The BMCLAPI maven variant of a maven URL
/// (`McLibNetFilesFromTokens`): everything before the first
/// "maven" is replaced by the BMCLAPI root, then the fabric/
/// forge/neoforged host spellings are normalized to plain "maven"
/// (BMCLAPI's layout).
fn maven_mirror_variant(url: &str) -> String {
    let Some(idx) = url.find("maven") else {
        return url.to_string();
    };
    format!("{BMCLAPI_ROOT}/{}", &url[idx..])
        .replace("maven.fabricmc.net", "maven")
        .replace("maven.minecraftforge.net", "maven")
        .replace("maven.neoforged.net/releases", "maven")
}

/// Asset-index facts of a resolved instance JSON (`McAssetsGetIndex`
/// / `McAssetsGetIndexName`), with the upstream legacy fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetIndexInfo {
    pub id: String,
    pub url: Option<String>,
    pub sha1: Option<String>,
    pub size: i64,
}

/// The legacy fallback index PCL uses when a version JSON has no
/// `assetIndex` at all (`McAssetsGetIndex`, ReturnLegacyOnError).
pub const LEGACY_INDEX_SHA1: &str = "c0fd82dc40abfbc17d7619f5b94a5d77dbcf5b95";
pub const LEGACY_INDEX_URL: &str =
    "https://launchermeta.mojang.com/v1/packages/770572e819335b6e76850552116b140ded74ecab/legacy.json";

pub fn asset_index_info(resolved_json: &Value) -> AssetIndexInfo {
    if let Some(index) = resolved_json.get("assetIndex") {
        return AssetIndexInfo {
            id: token_string(index.get("id")).unwrap_or_else(|| "legacy".to_string()),
            url: token_string(index.get("url")),
            sha1: token_string(index.get("sha1")),
            size: index.get("size").and_then(Value::as_i64).unwrap_or(-1),
        };
    }
    if let Some(assets) = token_string(resolved_json.get("assets")) {
        return AssetIndexInfo {
            id: assets,
            url: None,
            sha1: None,
            size: -1,
        };
    }
    AssetIndexInfo {
        id: "legacy".to_string(),
        url: Some(LEGACY_INDEX_URL.to_string()),
        sha1: Some(LEGACY_INDEX_SHA1.to_string()),
        size: -1,
    }
}

/// `DlClientAssetIndexGet`: the index file download, if any URL is
/// known (checker is the upstream `IsJson` one).
pub fn asset_index_net_file(
    info: &AssetIndexInfo,
    mc_folder: &Path,
    pref: SourcePreference,
) -> Option<NetFile> {
    let url = info.url.as_ref()?;
    Some(NetFile {
        local_path: mc_folder
            .join("assets")
            .join("indexes")
            .join(format!("{}.json", info.id)),
        sources: dl_source_launcher_or_meta_get(pref, url),
        checker: FileChecker {
            is_json: true,
            ..FileChecker::default()
        },
    })
}

/// `McAssetsListGet`: the object downloads for one index JSON text.
/// `mc_folder` and `indie_dir` are native paths. `check_hash`
/// mirrors the upstream flag: with it off, existing files of the
/// right size are skipped instead of re-verified (to unlimited
/// size semantics — upstream compares `FileSize` only).
pub fn asset_net_files(
    index_text: &str,
    mc_folder: &Path,
    indie_dir: &Path,
    check_hash: bool,
    pref: SourcePreference,
) -> Result<Vec<NetFile>, String> {
    let index: Value =
        serde_json::from_str(index_text).map_err(|e| format!("资源索引解析失败：{e}"))?;
    let map_to_resources = index
        .get("map_to_resources")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let virtual_ = index
        .get("virtual")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let objects = index
        .get("objects")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut files = Vec::new();
    for (name, data) in &objects {
        let Some(hash) = token_string(data.get("hash")) else {
            continue;
        };
        if hash.len() < 2 {
            continue;
        }
        let size = data.get("size").and_then(Value::as_i64).unwrap_or(0);
        let local = if map_to_resources {
            indie_dir.join("resources").join(name.replace('/', &std::path::MAIN_SEPARATOR.to_string()))
        } else if virtual_ {
            mc_folder
                .join("assets")
                .join("virtual")
                .join("legacy")
                .join(name.replace('/', &std::path::MAIN_SEPARATOR.to_string()))
        } else {
            mc_folder
                .join("assets")
                .join("objects")
                .join(&hash[..2])
                .join(&hash)
        };
        if !check_hash {
            // Skip files already present with the expected size.
            if let Ok(meta) = std::fs::metadata(&local) {
                if meta.len() as i64 == size {
                    continue;
                }
            }
        }
        let official = format!(
            "https://resources.download.minecraft.net/{}/{}",
            &hash[..2],
            hash
        );
        files.push(NetFile {
            local_path: local,
            sources: dl_source_assets_get(pref, &official),
            checker: FileChecker {
                min_size: -1,
                actual_size: if size == 0 { -1 } else { size },
                hash: Some(hash),
                is_json: false,
            },
        });
    }
    Ok(files)
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MC: &str = r"C:\MC\";

    #[test]
    fn client_jar_plan() {
        let resolved = json!({
            "downloads": {"client": {
                "url": "https://piston-data.mojang.com/v1/objects/abc/client.jar",
                "sha1": "0123456789012345678901234567890123456789",
                "size": 12345
            }}
        });
        let file = client_jar_net_file(
            &resolved,
            "1.20.1",
            Path::new("C:/MC"),
            SourcePreference::OfficialFirst,
        )
        .unwrap();
        assert_eq!(file.checker.min_size, 1024);
        assert_eq!(file.checker.actual_size, 12345);
        assert_eq!(
            file.checker.hash.as_deref(),
            Some("0123456789012345678901234567890123456789")
        );
        assert_eq!(
            file.sources[0],
            "https://piston-data.mojang.com/v1/objects/abc/client.jar"
        );
        assert_eq!(
            file.sources[1],
            "https://bmclapi2.bangbang93.com/v1/objects/abc/client.jar"
        );
        assert!(client_jar_net_file(&json!({}), "x", Path::new("C:/MC"), SourcePreference::OfficialFirst).is_none());
    }

    #[test]
    fn library_plans_maven_variant_and_defaults() {
        let fabric = LibEntry::new(
            Some("net.fabricmc:fabric-loader:0.15.0".to_string()),
            format!(
                "{MC}libraries\\net\\fabricmc\\fabric-loader\\0.15.0\\fabric-loader-0.15.0.jar"
            ),
            100,
            false,
            Some("aaaa".to_string()),
            Some(
                "https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar"
                    .to_string(),
            ),
        );
        let files = library_net_files(&[fabric], MC, SourcePreference::MirrorFirst);
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].sources[0],
            "https://bmclapi2.bangbang93.com/maven/net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar"
        );
        assert_eq!(files[0].checker.actual_size, 100);

        // No URL in the JSON: fall back to libraries.minecraft.net.
        let guava = LibEntry::new(
            Some("com.google.guava:guava:31.1".to_string()),
            format!("{MC}libraries\\com\\google\\guava\\guava\\31.1\\guava-31.1.jar"),
            0,
            false,
            None,
            None,
        );
        let files = library_net_files(&[guava], MC, SourcePreference::OfficialFirst);
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].sources[0],
            "https://libraries.minecraft.net/com/google/guava/guava/31.1/guava-31.1.jar"
        );
        assert_eq!(files[0].checker.actual_size, -1, "size 0 means unchecked");

        // LabyMod: no checking at all.
        let laby = LibEntry::new(
            Some("net.labymod:laby:1".to_string()),
            format!("{MC}libraries\\net\\labymod\\laby\\1\\laby-1.jar"),
            55,
            false,
            Some("bbbb".to_string()),
            None,
        );
        let files = library_net_files(&[laby], MC, SourcePreference::OfficialFirst);
        assert_eq!(files[0].checker, FileChecker::default());
    }

    #[test]
    fn asset_index_and_objects() {
        // Legacy fallback when the JSON has no assetIndex at all.
        let info = asset_index_info(&json!({"id": "1.20.1"}));
        assert_eq!(info.id, "legacy");
        assert_eq!(info.url.as_deref(), Some(LEGACY_INDEX_URL));
        let info = asset_index_info(&json!({
            "assetIndex": {"id": "5", "url": "https://example.invalid/5.json", "sha1": "x", "size": 10}
        }));
        assert_eq!(info.id, "5");

        let hash = "aa".repeat(20);
        let index = format!(
            "{{\"objects\":{{\"icons/icon_16x16.png\":{{\"hash\":\"{hash}\",\"size\":10}}}}}}"
        );
        let files = asset_net_files(
            &index,
            Path::new("/mc"),
            Path::new("/mc"),
            true,
            SourcePreference::OfficialFirst,
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0]
            .local_path
            .ends_with(format!("assets/objects/aa/{hash}")));
        assert_eq!(files[0].checker.hash.as_deref(), Some(hash.as_str()));

        // virtual indexes land under assets/virtual/legacy.
        let vindex = format!(
            "{{\"virtual\":true,\"objects\":{{\"a.txt\":{{\"hash\":\"{hash}\",\"size\":1}}}}}}"
        );
        let files = asset_net_files(
            &vindex,
            Path::new("/mc"),
            Path::new("/mc"),
            true,
            SourcePreference::OfficialFirst,
        )
        .unwrap();
        assert!(files[0]
            .local_path
            .ends_with("assets/virtual/legacy/a.txt"));
    }

    #[test]
    fn path_round_trip() {
        #[cfg(not(target_os = "windows"))]
        {
            let native = Path::new("/home/user/.minecraft");
            assert_eq!(to_native_path(&to_pcl_path(native)), native.to_path_buf());
        }
        // Windows-form paths keep their root (M2 real-machine
        // regression: every classpath entry lost it and came out
        // drive-relative — `C:Users\...` — so the JVM could not
        // load the main class). Compared in string form with the
        // host separator so the assertions state the Windows
        // semantics on any host.
        let sep = std::path::MAIN_SEPARATOR;
        let shown = |p: &str| to_native_path(p).to_string_lossy().into_owned();
        // Drive-absolute: root separator right after the drive.
        assert_eq!(
            shown(r"C:\MC\libraries\a.jar"),
            format!("C:{sep}MC{sep}libraries{sep}a.jar")
        );
        // Forward slashes in the input convert the same way.
        assert_eq!(
            shown("C:/MC/libraries/a.jar"),
            format!("C:{sep}MC{sep}libraries{sep}a.jar")
        );
        // The drive root itself, and a single subfolder.
        assert_eq!(shown(r"C:\"), format!("C:{sep}"));
        assert_eq!(shown(r"D:\game"), format!("D:{sep}game"));
        // Drive-relative stays drive-relative, as written.
        assert_eq!(shown(r"C:foo\bar"), format!("C:foo{sep}bar"));
        // UNC: server and share are part of the root.
        assert_eq!(
            shown(r"\\server\share\dir\file.jar"),
            format!("{sep}{sep}server{sep}share{sep}dir{sep}file.jar")
        );
        assert_eq!(
            shown(r"\\server\share"),
            format!("{sep}{sep}server{sep}share")
        );
        // Rooted without a drive, and plain relative paths.
        assert_eq!(shown(r"\home\user"), format!("{sep}home{sep}user"));
        assert_eq!(shown(r"a\b\c"), format!("a{sep}b{sep}c"));
        assert_eq!(shown(""), "");
        // Exact round trip for a well-formed Windows path.
        assert_eq!(
            to_pcl_path(&to_native_path(r"C:\MC\libraries\a.jar")),
            r"C:\MC\libraries\a.jar"
        );
    }
}
