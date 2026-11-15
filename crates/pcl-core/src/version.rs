//! Minecraft version JSON model + PCL version-name semantics.
//!
//! The JSON model follows the Mojang version JSON format that PCL parses
//! when scanning instances (`Plain Craft Launcher 2/Modules/Minecraft/
//! ModMinecraft.vb`) and when assembling a launch (`ModLaunch.vb`).
//! The name semantics (`name_to_version`, `drop_ordinal`) are a faithful
//! port of `PCLCS/Minecraft/McVersion.cs` (`NameToVersion`, `Drop`),
//! including its April-Fools-version mappings and the legacy `1.x`
//! renumbering (e.g. `1.20.1` becomes the comparable triple `(20, 0, 1)`).

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Mojang version JSON model (subset PCL actually reads; unknown fields are
// ignored so real-world version JSONs — vanilla, Forge, Fabric — all parse).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    /// Parent version id this one inherits libraries/arguments from.
    pub inherits_from: Option<String>,
    #[serde(rename = "type")]
    pub version_type: String,
    pub release_time: Option<String>,
    pub time: Option<String>,
    pub main_class: Option<String>,
    pub assets: Option<String>,
    pub asset_index: Option<AssetIndex>,
    pub downloads: Option<Downloads>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    pub arguments: Option<Arguments>,
    /// Legacy (pre-1.13) single-string argument template.
    pub minecraft_arguments: Option<String>,
    pub java_version: Option<JavaVersion>,
    /// Minimum launcher version field present in newer manifests.
    pub minimum_launcher_version: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndex {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    pub total_size: Option<u64>,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Downloads {
    pub client: Option<Artifact>,
    pub server: Option<Artifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub path: Option<String>,
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibraryDownloads>,
    pub rules: Option<Vec<Rule>>,
    pub natives: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryDownloads {
    pub artifact: Option<Artifact>,
    pub classifiers: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub action: String,
    pub os: Option<RuleOs>,
    pub features: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleOs {
    pub name: Option<String>,
    pub version: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Arguments {
    /// Mixed string / conditional-object entries, kept as raw values at
    /// this layer; the launch module interprets them.
    #[serde(default)]
    pub game: Vec<serde_json::Value>,
    #[serde(default)]
    pub jvm: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    pub component: String,
    pub major_version: u32,
}

impl VersionJson {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The comparable vanilla version derived from this JSON's id,
    /// following PCL's semantics.
    pub fn comparable_version(&self) -> ComparableVersion {
        name_to_version(&self.id)
    }
}

// ---------------------------------------------------------------------------
// PCL version-name semantics — ported from PCLCS McVersion.NameToVersion.
// ---------------------------------------------------------------------------

/// Comparable three-part vanilla version. The unknown/old-snapshot
/// sentinel is `(9999, 0, 0)`, exactly as in PCL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComparableVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ComparableVersion {
    pub const UNKNOWN: Self = Self {
        major: 9999,
        minor: 0,
        patch: 0,
    };

    /// PCL `McVersion.Valid`: a version is valid when it is not the
    /// unknown sentinel range.
    pub fn is_valid(&self) -> bool {
        self.major < 1000
    }

    /// PCL `McVersion.Drop` / `VersionToDrop`: ordering ordinal,
    /// `major * 10 + minor` for valid versions, `209` otherwise.
    pub fn drop_ordinal(&self) -> u32 {
        if self.is_valid() {
            self.major * 10 + self.minor
        } else {
            209
        }
    }
}

/// Leading ASCII digits of a segment, as PCL's `ParseSegment` does
/// (trailing preview markers are ignored; no digits means 0).
fn parse_segment(segments: &[&str], index: usize) -> u32 {
    let Some(seg) = segments.get(index) else {
        return 0;
    };
    let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

/// Faithful port of PCL `McVersion.NameToVersion`.
pub fn name_to_version(name: &str) -> ComparableVersion {
    let name = name
        .to_lowercase()
        .replace("_unobfuscated", "")
        .replace(" unobfuscated", "");
    let name = name.as_str();

    // April Fools versions map to the release they were based on.
    if name.starts_with("2.0") {
        return name_to_version("1.5.1");
    }
    if name == "15w14a" {
        return name_to_version("1.8.3");
    }
    if name.contains(".rv-pre") {
        return name_to_version("1.9.2");
    }
    if name.contains("shareware") {
        return name_to_version("1.13.2");
    }
    if name.starts_with("20w14") && name != "20w14a" {
        return name_to_version("1.15.2");
    }
    if name.contains("oneblockatatime") {
        return name_to_version("1.18.2");
    }
    // NOTE: the `!= "20w13a"` comparison reproduces the original C#
    // verbatim (it looks like a typo for "23w13a_or_b" handling, but a
    // port keeps observable behaviour identical; flagged in PORT-PLAN).
    if name.contains("23w13a") && name != "20w13a" {
        return name_to_version("1.19.4");
    }
    if name == "24w14potato" {
        return name_to_version("1.20.4");
    }
    if name == "25w14craftmine" {
        return name_to_version("1.21.4");
    }
    if name == "26w14a" {
        return name_to_version("26.1.1");
    }

    let segments: Vec<&str> = name
        .split(|c| [' ', '_', '-', '.'].contains(&c))
        .collect();
    if name.starts_with("1.") {
        // Legacy format: the minor segment moves into the major slot,
        // e.g. 1.20.1 -> (20, 0, 1).
        ComparableVersion {
            major: parse_segment(&segments, 1),
            minor: 0,
            patch: parse_segment(&segments, 2),
        }
    } else if name.len() >= 3
        && name.as_bytes()[0].is_ascii_digit()
        && name.as_bytes()[0] != b'0'
        && name.as_bytes()[0] != b'1'
        && name.as_bytes()[1].is_ascii_digit()
        && name.as_bytes()[2] == b'.'
    {
        // New format (26.x and later): ^[2-9][0-9]\.
        ComparableVersion {
            major: parse_segment(&segments, 0),
            minor: parse_segment(&segments, 1),
            patch: parse_segment(&segments, 2),
        }
    } else {
        ComparableVersion::UNKNOWN
    }
}

/// Convenience wrapper mirroring PCL `McVersion.VersionToDrop`.
pub fn version_to_drop(name: &str) -> u32 {
    name_to_version(name).drop_ordinal()
}

// ---------------------------------------------------------------------------
// Vanilla-name detection — ported from PCLCS McVersion.GetVanillaName and
// the post-processing in the McVersion(instanceName, json, jarVersionJson)
// constructor.
// ---------------------------------------------------------------------------

/// MeloongCore `RegexSeek`: the first regex match in `text`, or None.
/// .NET patterns may use lookbehind, which the Rust `regex` crate does
/// not support; the affected patterns below are transcribed with the
/// lookbehind prefix as a non-capturing group plus one capture group,
/// and read through [`regex_seek_group1`] instead.
fn regex_seek(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).expect("constant pattern");
    re.find(text).map(|m| m.as_str().to_string())
}

/// Like [`regex_seek`] but returns capture group 1 (the part that
/// followed the lookbehind prefix in the .NET original).
fn regex_seek_group1(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).expect("constant pattern");
    re.captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// JToken-to-string following the same conversion the port uses for JSON
/// fields (strings pass through, null/missing is None, other scalars
/// render as compact JSON text).
fn token_text(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// PCL `McVersion.FindArgumentValue`: the token following `name` in
/// `arguments.game`, as a string; None when absent or last.
fn find_argument_value(json: &serde_json::Value, name: &str) -> Option<String> {
    let game = json.get("arguments")?.get("game")?.as_array()?;
    for i in 0..game.len().saturating_sub(1) {
        if token_text(game.get(i)).as_deref() == Some(name) {
            return token_text(game.get(i + 1));
        }
    }
    None
}

/// The release year parsed from the JSON's `releaseTime`, mirroring the
/// constructor's `ToObject::<DateTime>()` (whose failure leaves the
/// 1970 default, i.e. "not an old version"). Only the leading
/// ISO-style year is read; unparseable values behave like the original's
/// caught exception and yield None.
fn release_year(json: &serde_json::Value) -> Option<i32> {
    let text = token_text(json.get("releaseTime"))?;
    let prefix: String = text.chars().take(4).collect();
    if prefix.len() == 4 && prefix.bytes().all(|b| b.is_ascii_digit()) {
        prefix.parse().ok()
    } else {
        None
    }
}

/// Outcome of vanilla-name detection: the detected name plus whether
/// the evidence was conclusive (PCL's `McVersion.Reliable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VanillaNameDetection {
    pub name: String,
    pub reliable: bool,
}

/// The pattern PCL scans ids / folder names / JSON text with
/// (`VersionPattern`, matched case-insensitively).
const VERSION_PATTERN: &str = r"(([1-9][0-9]w[0-9]{2}[a-g])|((1|[2-9][0-9])\.[0-9]+(\.[0-9]+)?(-(pre|rc|snapshot-?)[1-9]*| Pre-Release( [1-9])?)?))(_unobfuscated)?";

/// Faithful port of `McVersion.GetVanillaName` plus the constructor's
/// finishing steps (a null result becomes "Unknown" and the
/// `_unobfuscated` / ` Unobfuscated` suffixes are stripped).
///
/// `json` is the instance's inheritance-resolved version JSON (as
/// `McInstance.JsonObject` hands to the McVersion constructor);
/// `jar_version_json` is the `version.json` inside the instance jar,
/// when available.
///
/// Faithfulness notes:
/// - Two spots in the original dereference a possibly-missing token
///   (`json["libraries"]!`, `json["id"]!`) and throw; the exception
///   propagates to `McInstance.Version`, which falls back to
///   `McVersion("Unknown")` (Reliable = true). The port returns exactly
///   that outcome instead of panicking.
/// - The original searches the `libraries` array's indented rendering;
///   the port searches the compact rendering — every pattern is local
///   to a single token, so this is equivalent.
pub fn get_vanilla_name(
    instance_name: &str,
    json: &serde_json::Value,
    jar_version_json: Option<&serde_json::Value>,
) -> VanillaNameDetection {
    let detected = detect_vanilla_name(instance_name, json, jar_version_json);
    match detected {
        Ok((raw, reliable)) => VanillaNameDetection {
            name: raw.replace("_unobfuscated", "").replace(" Unobfuscated", ""),
            reliable,
        },
        // A null-dereference spot in the original (see the doc comment):
        // McInstance.Version falls back to McVersion("Unknown").
        Err(()) => VanillaNameDetection {
            name: "Unknown".to_string(),
            reliable: true,
        },
    }
}

/// The detection chain itself. Ok((name, reliable)) mirrors a normal
/// return (name may still be None-equivalent when nothing matched at
/// all — represented as Ok with an empty marker via `None` inside);
/// Err(()) marks the original's throwing paths.
fn detect_vanilla_name(
    instance_name: &str,
    json: &serde_json::Value,
    jar_version_json: Option<&serde_json::Value>,
) -> Result<(String, bool), ()> {
    // Old versions are recognized by release year alone (2000 < y < 2013).
    if let Some(year) = release_year(json) {
        if year > 2000 && year < 2013 {
            return Ok(("Old".to_string(), true));
        }
    }
    // Experimental snapshots.
    if token_text(json.get("type")).as_deref() == Some("pending") {
        return Ok(("pending".to_string(), true));
    }
    // PCL-downloaded versions carry clientVersion.
    if let Some(client_version) = token_text(json.get("clientVersion")) {
        return Ok((client_version, true));
    }
    // HMCL-downloaded versions: the "game" patch's version.
    if let Some(patches) = json.get("patches").and_then(serde_json::Value::as_array) {
        let game_patch = patches
            .iter()
            .find(|p| token_text(p.get("id")).as_deref() == Some("game"));
        if let Some(version) = game_patch.and_then(|p| token_text(p.get("version"))) {
            return Ok((version, true));
        }
    }
    // Forge / NeoForge arguments.
    if let Some(fml) = find_argument_value(json, "--fml.mcVersion") {
        if !fml.is_empty() {
            return Ok((fml, true));
        }
    }
    // The inherited version (LiteLoader prefers its jar entry).
    let inherit = token_text(json.get("inheritsFrom")).unwrap_or_default();
    if !inherit.is_empty() {
        let jar = token_text(json.get("jar")).unwrap_or_default();
        return Ok((if jar.is_empty() { inherit } else { jar }, true));
    }
    // The client download URL.
    if let Some(downloads) = json.get("downloads") {
        let text = serde_json::to_string(downloads);
        if let Ok(text) = text {
            if let Some(found) =
                regex_seek_group1(&text, r"(?:launcher.mojang.com/mc/game/)([^/]*)")
            {
                return Ok((found, true));
            }
        }
    }
    // Forge / OptiFine / Fabric library coordinates. A missing
    // `libraries` key throws in the original (see get_vanilla_name).
    let Some(libraries) = json.get("libraries") else {
        return Err(());
    };
    let libraries_text = serde_json::to_string(libraries).unwrap_or_default();
    const LIBRARY_PATTERNS: [&str; 4] = [
        r"(?:net.minecraftforge:forge:)([0-9]{1,2}.[0-9+.]+)",
        r"(?:net.minecraftforge:fmlloader:)([0-9]{1,2}.[0-9+.]+)",
        r"(?:optifine:OptiFine:)([0-9]{1,2}.[0-9+.]+)",
        r#"(?:(?:fabricmc|quiltmc):intermediary:)([^"]*)"#,
    ];
    for pattern in LIBRARY_PATTERNS {
        if let Some(found) = regex_seek_group1(&libraries_text, pattern) {
            return Ok((found, true));
        }
    }
    // The jar entry.
    if let Some(jar) = token_text(json.get("jar")) {
        return Ok((jar, true));
    }
    // The jar's own version.json (names of 32+ chars are distrusted —
    // per the original comment they may be a hash).
    if let Some(jar_name) = jar_version_json.and_then(|j| token_text(j.get("name"))) {
        if jar_name.chars().count() < 32 {
            return Ok((jar_name, true));
        }
    }
    let pattern = format!("(?i){VERSION_PATTERN}");
    // The JSON's own id.
    let Some(id) = token_text(json.get("id")) else {
        return Err(()); // json["id"]! in the original
    };
    if let Some(found) = regex_seek(&id, &pattern) {
        return Ok((found, true));
    }
    // Everything below is guesswork: the original flags it unreliable.
    if let Some(found) = regex_seek(instance_name, &pattern) {
        return Ok((found, false));
    }
    let mut without_libraries = json.clone();
    if let Some(obj) = without_libraries.as_object_mut() {
        obj.remove("libraries");
    }
    let text = serde_json::to_string(&without_libraries).unwrap_or_default();
    match regex_seek(&text, &pattern) {
        Some(found) => Ok((found, false)),
        // The original returns null here; the constructor substitutes
        // "Unknown".
        None => Ok(("Unknown".to_string(), false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_shift_minor_into_major() {
        assert_eq!(
            name_to_version("1.20.1"),
            ComparableVersion { major: 20, minor: 0, patch: 1 }
        );
        assert_eq!(
            name_to_version("1.12.2"),
            ComparableVersion { major: 12, minor: 0, patch: 2 }
        );
        assert_eq!(name_to_version("1.21.5").drop_ordinal(), 210);
    }

    #[test]
    fn new_format_names_parse_directly() {
        assert_eq!(
            name_to_version("26.1"),
            ComparableVersion { major: 26, minor: 1, patch: 0 }
        );
        assert_eq!(
            name_to_version("26.3.2"),
            ComparableVersion { major: 26, minor: 3, patch: 2 }
        );
        assert_eq!(name_to_version("26.3.2").drop_ordinal(), 263);
    }

    #[test]
    fn snapshots_and_fools_versions() {
        // Old weekly snapshots carry no dotted number -> unknown sentinel.
        assert_eq!(name_to_version("16w01a"), ComparableVersion::UNKNOWN);
        assert_eq!(version_to_drop("16w01a"), 209);
        // April Fools mappings from the original table.
        assert_eq!(
            name_to_version("20w14∞"),
            ComparableVersion { major: 15, minor: 0, patch: 2 }
        );
        assert_eq!(
            name_to_version("24w14potato"),
            ComparableVersion { major: 20, minor: 0, patch: 4 }
        );
        assert_eq!(
            name_to_version("26w14a"),
            ComparableVersion { major: 26, minor: 1, patch: 1 }
        );
    }

    #[test]
    fn parses_vanilla_version_json_fixture() {
        let text = include_str!("../tests/fixtures/version-1.20.1.json");
        let v = VersionJson::parse(text).expect("fixture must parse");
        assert_eq!(v.id, "1.20.1");
        assert_eq!(v.version_type, "release");
        assert_eq!(v.main_class.as_deref(), Some("net.minecraft.client.main.Main"));
        assert_eq!(v.libraries.len(), 3);
        assert_eq!(v.libraries[0].name, "org.lwjgl:lwjgl:3.3.2");
        let client = v.downloads.as_ref().and_then(|d| d.client.as_ref()).unwrap();
        assert_eq!(client.sha1, "0c3ec58475062272fa95129fc368413ba57d166a");
        assert_eq!(client.size, 23_028_853);
        let idx = v.asset_index.as_ref().unwrap();
        assert_eq!(idx.id, "5");
        assert_eq!(
            v.comparable_version(),
            ComparableVersion { major: 20, minor: 0, patch: 1 }
        );
        let args = v.arguments.as_ref().unwrap();
        assert_eq!(args.game.len(), 3);
        assert_eq!(v.java_version.as_ref().unwrap().major_version, 17);
    }

    fn detect(json: serde_json::Value) -> VanillaNameDetection {
        get_vanilla_name("some-instance", &json, None)
    }

    #[test]
    fn vanilla_name_from_client_version_and_old_release() {
        // clientVersion (PCL-downloaded JSON) wins outright.
        let d = detect(serde_json::json!({
            "id": "custom-name", "clientVersion": "1.20.1", "libraries": []
        }));
        assert_eq!(d.name, "1.20.1");
        assert!(d.reliable);
        // A 2000 < year < 2013 releaseTime short-circuits to "Old",
        // ahead of every other source.
        let d = detect(serde_json::json!({
            "id": "b1.7.3", "releaseTime": "2011-07-08T00:00:00+00:00",
            "clientVersion": "1.20.1", "libraries": []
        }));
        assert_eq!(d.name, "Old");
        assert!(d.reliable);
        // type "pending" is returned verbatim.
        let d = detect(serde_json::json!({
            "id": "x", "type": "pending", "libraries": []
        }));
        assert_eq!(d.name, "pending");
    }

    #[test]
    fn vanilla_name_from_inherit_jar_and_fml() {
        // inheritsFrom is used when no jar entry overrides it.
        let d = detect(serde_json::json!({
            "id": "fabric-loader-0.16.9-1.20.1", "inheritsFrom": "1.20.1",
            "libraries": []
        }));
        assert_eq!(d.name, "1.20.1");
        // LiteLoader-style: the jar entry beats inheritsFrom.
        let d = detect(serde_json::json!({
            "id": "liteloader-1.5.2", "inheritsFrom": "1.5.2", "jar": "1.5.2",
            "libraries": []
        }));
        assert_eq!(d.name, "1.5.2");
        // --fml.mcVersion in arguments.game beats inheritsFrom.
        let d = detect(serde_json::json!({
            "id": "forge-1.20.1", "inheritsFrom": "1.20.1",
            "arguments": {"game": ["--fml.mcVersion", "1.20.1", "--other", "x"]},
            "libraries": []
        }));
        assert_eq!(d.name, "1.20.1");
        assert!(d.reliable);
    }

    #[test]
    fn vanilla_name_from_libraries_downloads_and_id() {
        // Forge library coordinate.
        let d = detect(serde_json::json!({
            "id": "forge",
            "libraries": [{"name": "net.minecraftforge:forge:1.20.1-47.1.0"}]
        }));
        assert_eq!(d.name, "1.20.1");
        // Fabric intermediary coordinate.
        let d = detect(serde_json::json!({
            "id": "fabric",
            "libraries": [{"name": "net.fabricmc:intermediary:1.21.3"}]
        }));
        assert_eq!(d.name, "1.21.3");
        // Mojang download URL.
        let d = detect(serde_json::json!({
            "id": "x",
            "downloads": {"client": {"url": "https://launcher.mojang.com/mc/game/1.16.5/client/abc/client.jar"}},
            "libraries": []
        }));
        assert_eq!(d.name, "1.16.5");
        // Fall back to the id pattern (reliable), then to the folder
        // name (unreliable).
        let d = detect(serde_json::json!({"id": "24w14a", "libraries": []}));
        assert_eq!(d.name, "24w14a");
        assert!(d.reliable);
        // The VersionPattern does not match fool-version ids like
        // "24w14potato" (no [a-g] after the week digits, no dotted
        // number); with no other evidence the original ends at
        // Unknown/unreliable for such a bare JSON.
        let d = detect(serde_json::json!({"id": "24w14potato", "libraries": []}));
        assert_eq!(d.name, "Unknown");
        assert!(!d.reliable);
        let d = get_vanilla_name(
            "my-1.19.4-instance",
            &serde_json::json!({"id": "custom", "libraries": []}),
            None,
        );
        assert_eq!(d.name, "1.19.4");
        assert!(!d.reliable);
    }

    #[test]
    fn vanilla_name_unknown_paths() {
        // Missing `libraries` dereferences null in the original; the
        // McInstance.Version fallback yields Unknown / reliable.
        let d = detect(serde_json::json!({"id": "1.20.1"}));
        assert_eq!(d.name, "Unknown");
        assert!(d.reliable);
        // Nothing matches anywhere: Unknown, unreliable.
        let d = get_vanilla_name(
            "plain-name",
            &serde_json::json!({"id": "custom", "libraries": [], "type": "release"}),
            None,
        );
        assert_eq!(d.name, "Unknown");
        assert!(!d.reliable);
        // The jar's version.json name is used when short enough...
        let d = get_vanilla_name(
            "plain-name",
            &serde_json::json!({"id": "custom", "libraries": []}),
            Some(&serde_json::json!({"name": "1.8.9"})),
        );
        assert_eq!(d.name, "1.8.9");
        assert!(d.reliable);
        // ...but a 32+ char name (possibly a hash) is skipped.
        let d = get_vanilla_name(
            "plain-name",
            &serde_json::json!({"id": "custom", "libraries": []}),
            Some(&serde_json::json!({"name": "0123456789abcdef0123456789abcdef"})),
        );
        assert_eq!(d.name, "Unknown");
        // The _unobfuscated suffix is stripped by the constructor step.
        let d = detect(serde_json::json!({"id": "1.21.5_unobfuscated", "libraries": []}));
        assert_eq!(d.name, "1.21.5");
    }

    #[test]
    fn parses_inherited_loader_json() {
        // Fabric-style JSON: inheritsFrom + sparse fields must still parse.
        let text = r#"{
            "id": "fabric-loader-0.16.9-1.20.1",
            "inheritsFrom": "1.20.1",
            "type": "release",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": [
                {"name": "net.fabricmc:fabric-loader:0.16.9",
                 "downloads": {"artifact": {"path": "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar",
                     "sha1": "0000000000000000000000000000000000000000", "size": 1, "url": "https://example.invalid/x.jar"}}}
            ],
            "arguments": {"game": [], "jvm": []}
        }"#;
        let v = VersionJson::parse(text).expect("loader json must parse");
        assert_eq!(v.inherits_from.as_deref(), Some("1.20.1"));
        assert_eq!(v.libraries.len(), 1);
    }
}
