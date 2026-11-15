//! The Mojang version manifest: fetch, disk cache and lookup.
//!
//! Ports of `ModDownload.vb`'s `DlClientList*` loaders and
//! `DlClientListGet`:
//! - Official source: `https://launchermeta.mojang.com/mc/game/version_manifest.json`
//!   (what PCL's Mojang loader requests; the payload is the v2
//!   schema — per-version `sha1` included — despite the v1 file
//!   name) plus the canonical piston-meta v2 URL as a second
//!   official candidate.
//! - Mirror source: `https://bmclapi2.bangbang93.com/mc/game/version_manifest.json`.
//! - Both loaders validate `versions.len() >= 200` ("获取到的版本
//!   列表长度不足").
//! - The Mojang loader also measures its own latency: under
//!   4000 ms sets `DlPreferMojang`, which `DlSourcePreferMojang`
//!   consults in Auto mode. The port returns that measurement as
//!   [`ManifestOutcome::official_fast`].
//! - Upstream races the two loaders with per-setting delays
//!   (`DlSourceLoader`); the port tries them in preference order
//!   with fallback, which has the same outcome whenever the first
//!   source answers. The disk cache is a port-level addition for
//!   offline use: a fresh fetch overwrites it, and it is only read
//!   when every source fails.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::download::SourcePreference;
use crate::http::{HttpClient, HttpRequest};

/// Official manifest URLs, in the order PCL would reach them.
pub const OFFICIAL_MANIFEST_URLS: &[&str] = &[
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
    "https://launchermeta.mojang.com/mc/game/version_manifest.json",
];
/// BMCLAPI mirror manifest URL (`DlClientListBmclapiMain`).
pub const MIRROR_MANIFEST_URL: &str = "https://bmclapi2.bangbang93.com/mc/game/version_manifest.json";

/// One `versions[]` entry of the manifest.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestVersion {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(rename = "releaseTime", default)]
    pub release_time: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
}

/// The version manifest (`version_manifest_v2` schema).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    #[serde(default)]
    pub versions: Vec<ManifestVersion>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

impl VersionManifest {
    /// Parse manifest JSON text (`DeserializeJson` in the loaders).
    pub fn parse(text: &str) -> Result<VersionManifest, String> {
        serde_json::from_str(text).map_err(|e| format!("Minecraft 版本列表解析失败：{e}"))
    }

    /// The loaders' sanity check: a real manifest has ≥ 200
    /// versions; anything shorter is an error page in disguise.
    pub fn validate(&self) -> Result<(), String> {
        if self.versions.len() < 200 {
            return Err(format!(
                "获取到的版本列表长度不足（{} 条）",
                self.versions.len()
            ));
        }
        Ok(())
    }

    /// Find one version entry by id.
    pub fn find(&self, id: &str) -> Option<&ManifestVersion> {
        self.versions.iter().find(|v| v.id == id)
    }

    pub fn latest_release(&self) -> Option<&ManifestVersion> {
        self.find(&self.latest.release)
    }

    pub fn latest_snapshot(&self) -> Option<&ManifestVersion> {
        self.find(&self.latest.snapshot)
    }

    /// All release Drop ordinals, high → low (`DlClientListMain`'s
    /// `AllDrops` extraction: ids containing `-` are skipped, as is
    /// Drop 209).
    pub fn all_drops(&self) -> Vec<u32> {
        let mut drops: Vec<u32> = Vec::new();
        for version in &self.versions {
            if version.id.contains('-') {
                continue;
            }
            let drop = crate::version::name_to_version(&version.id).drop_ordinal();
            if drop == 209 {
                continue;
            }
            if !drops.contains(&drop) {
                drops.push(drop);
            }
        }
        drops.sort_by(|a, b| b.cmp(a));
        drops
    }
}

/// Normalize a user-typed version id the way `DlClientListGet`
/// does: `_` → `-`, and a trailing `.0` is dropped (except "1.0").
pub fn normalize_version_id(id: &str) -> String {
    let mut id = id.replace('_', "-");
    if id != "1.0" && id.ends_with(".0") {
        id.truncate(id.len() - 2);
    }
    id
}

/// Result of a manifest fetch.
#[derive(Debug, Clone)]
pub struct ManifestOutcome {
    pub manifest: VersionManifest,
    /// "Mojang 官方源" / "BMCLAPI" / "本地缓存", as in
    /// `DlClientListResult.SourceName`.
    pub source_name: String,
    pub is_official: bool,
    /// The `DlPreferMojang` measurement: true when an official
    /// fetch completed in under 4000 ms.
    pub official_fast: bool,
    /// True when every network source failed and the disk cache
    /// answered instead.
    pub from_cache: bool,
}

/// Where the manifest disk cache lives for one .minecraft folder
/// (`<mc>/versions/version_manifest_v2.json` is avoided — PCL keeps
/// caches under its temp dir; the port uses the caller-provided
/// cache dir, defaulting to the OS temp dir).
pub fn cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("version_manifest_v2.json")
}

/// Fetch the manifest following `pref` (file-download-independent:
/// this is the *version list* preference, `ToolDownloadVersion`),
/// updating the disk cache on success and falling back to it when
/// all sources fail.
pub fn fetch_manifest(
    http: &dyn HttpClient,
    pref: SourcePreference,
    cache_dir: &Path,
) -> Result<ManifestOutcome, String> {
    let official: Vec<&str> = OFFICIAL_MANIFEST_URLS.to_vec();
    let mirror = vec![MIRROR_MANIFEST_URL];
    let ordered: Vec<(&str, bool)> = if pref.prefer_official() {
        official
            .iter()
            .map(|u| (*u, true))
            .chain(mirror.iter().map(|u| (*u, false)))
            .collect()
    } else {
        mirror
            .iter()
            .map(|u| (*u, false))
            .chain(official.iter().map(|u| (*u, true)))
            .collect()
    };
    let mut errors: Vec<String> = Vec::new();
    for (url, is_official) in ordered {
        let start = std::time::Instant::now();
        match http.request_retry(&HttpRequest::get(url)) {
            Ok(resp) => {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                match VersionManifest::parse(&resp.text()).and_then(|m| {
                    m.validate()?;
                    Ok(m)
                }) {
                    Ok(manifest) => {
                        let _ = std::fs::create_dir_all(cache_dir);
                        let _ = std::fs::write(cache_path(cache_dir), &resp.body);
                        return Ok(ManifestOutcome {
                            manifest,
                            source_name: if is_official {
                                "Mojang 官方源".to_string()
                            } else {
                                "BMCLAPI".to_string()
                            },
                            is_official,
                            official_fast: is_official && elapsed_ms < 4000,
                            from_cache: false,
                        });
                    }
                    Err(e) => errors.push(format!("[{url}] {e}")),
                }
            }
            Err(e) => errors.push(format!("[{url}] {e}")),
        }
    }
    // All sources failed: fall back to the disk cache.
    if let Ok(text) = std::fs::read_to_string(cache_path(cache_dir)) {
        if let Ok(manifest) = VersionManifest::parse(&text) {
            if manifest.validate().is_ok() {
                return Ok(ManifestOutcome {
                    manifest,
                    source_name: "本地缓存".to_string(),
                    is_official: false,
                    official_fast: false,
                    from_cache: true,
                });
            }
        }
    }
    Err(format!("获取 Minecraft 版本列表失败：{}", errors.join("；")))
}

/// `DlClientListGet`: the version-JSON download URL for `id`, or
/// None when the manifest does not contain it. The caller refetches
/// the manifest once before concluding (upstream reloads the list
/// when the id is missing, #5195); that policy lives in the CLI,
/// this is the pure lookup.
pub fn version_json_url<'m>(manifest: &'m VersionManifest, id: &str) -> Option<&'m str> {
    let id = normalize_version_id(id);
    manifest.find(&id).map(|v| v.url.as_str())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{FakeHttp, HttpError};

    fn manifest_json(count: usize) -> String {
        let mut versions = String::new();
        for i in 0..count {
            if i > 0 {
                versions.push(',');
            }
            versions.push_str(&format!(
                "{{\"id\":\"v{i}\",\"type\":\"release\",\"url\":\"https://example.invalid/v{i}.json\",\"sha1\":\"abc\"}}"
            ));
        }
        // Make two entries realistic ids.
        let versions = versions
            .replace("\"id\":\"v0\"", "\"id\":\"1.20.1\"")
            .replace("\"id\":\"v1\"", "\"id\":\"24w01a\"");
        format!(
            "{{\"latest\":{{\"release\":\"1.20.1\",\"snapshot\":\"24w01a\"}},\"versions\":[{versions}]}}"
        )
    }

    #[test]
    fn parse_validate_find() {
        let manifest = VersionManifest::parse(&manifest_json(210)).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.latest_release().unwrap().id, "1.20.1");
        assert_eq!(manifest.latest_snapshot().unwrap().id, "24w01a");
        assert!(manifest.find("nope").is_none());
        let small = VersionManifest::parse(&manifest_json(12)).unwrap();
        assert!(small.validate().is_err(), "short manifests are rejected");
    }

    #[test]
    fn normalize_ids() {
        assert_eq!(normalize_version_id("1.20.1"), "1.20.1");
        assert_eq!(normalize_version_id("1.20_1"), "1.20-1");
        assert_eq!(normalize_version_id("1.20.0"), "1.20");
        assert_eq!(normalize_version_id("1.0"), "1.0");
    }

    #[test]
    fn fetch_prefers_mirror_first_and_caches() {
        let dir = std::env::temp_dir().join(format!("rerust-manifest-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let http = FakeHttp::new(vec![FakeHttp::ok_json(&manifest_json(210))]);
        let outcome = fetch_manifest(&http, SourcePreference::MirrorFirst, &dir).unwrap();
        assert_eq!(outcome.source_name, "BMCLAPI");
        assert!(!outcome.from_cache);
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, MIRROR_MANIFEST_URL);
        assert!(cache_path(&dir).is_file(), "fresh fetch updates the disk cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_falls_back_to_cache_when_network_is_down() {
        let dir = std::env::temp_dir().join(format!("rerust-manifest-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cache_path(&dir), manifest_json(205)).unwrap();
        // 3 sources × up to 3 retry attempts each.
        let script = (0..9)
            .map(|_| Err(HttpError::Transport("down".to_string())))
            .collect();
        let http = FakeHttp::new(script);
        let outcome = fetch_manifest(&http, SourcePreference::OfficialFirst, &dir).unwrap();
        assert!(outcome.from_cache);
        assert_eq!(outcome.manifest.versions.len(), 205);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
