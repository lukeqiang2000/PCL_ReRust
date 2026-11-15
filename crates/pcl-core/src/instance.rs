//! Version JSON inheritance resolution and library-list construction.
//!
//! Sources: `Plain Craft Launcher 2/Modules/Minecraft/ModMinecraft.vb`
//! (the `McInstance.JsonObject` getter's HMCL-patches and `inheritsFrom`
//! merging, `McJsonRuleCheck`, `McLibListGetWithJson`, `McLibGet`, the
//! `McLibEntry` class) and the `${classpath}` assembly inside
//! `ModLaunch.vb`'s `McLaunchArgumentsReplace`.
//!
//! The merge primitive is Newtonsoft.Json's `JObject.Merge` with its
//! default settings (MergeArrayHandling.Concat,
//! MergeNullValueHandling.Merge, ordinal property names), which is what
//! PCL's merging observably does.

use crate::pathutil;
use crate::version::get_vanilla_name;
use crate::vrange::vb_val;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Newtonsoft JObject.Merge (default settings).
// ---------------------------------------------------------------------------

/// Merge `source` into `target` with Newtonsoft defaults: objects merge
/// recursively, arrays concatenate (target items first), anything else —
/// including an explicit null — replaces the target value.
pub fn newton_merge(target: &mut Value, source: &Value) {
    match (&mut *target, source) {
        (Value::Object(t), Value::Object(s)) => {
            for (key, source_value) in s {
                match t.get_mut(key) {
                    Some(target_value) => newton_merge(target_value, source_value),
                    None => {
                        t.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
        (Value::Array(t), Value::Array(s)) => {
            t.extend(s.iter().cloned());
        }
        (t, s) => {
            *t = s.clone();
        }
    }
}

/// JToken-to-string as VB's implicit conversions do it: strings pass
/// through, null/missing becomes None, other scalars render as text.
pub(crate) fn token_string(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    }
}

fn inherits_from_of(json: &Value) -> String {
    token_string(json.get("inheritsFrom")).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// HMCL patches format (ModMinecraft.vb JsonObject getter).
// ---------------------------------------------------------------------------

/// Whether the JSON is in HMCL's `patches` format (has `patches`, no
/// `time`).
pub fn is_hmcl_format(json: &Value) -> bool {
    json.get("patches").is_some() && json.get("time").is_none()
}

/// Merge HMCL `patches` into one object: patches are sorted by their
/// numeric `priority` (ascending, `Val` semantics) and merged in order
/// with `newton_merge`; patches without an `id` are skipped. The result
/// gets `id` set to the instance name and any `inheritsFrom` removed.
/// Returns Err when no patch carries an id (the original ends up with a
/// null object and its getter throws).
pub fn merge_hmcl_patches(json: &Value, instance_name: &str) -> Result<Value, String> {
    let patches = json
        .get("patches")
        .and_then(Value::as_array)
        .ok_or("HMCL patches is not an array")?;
    let mut sorted: Vec<&Value> = patches.iter().collect();
    sorted.sort_by(|a, b| {
        let pa = vb_val(&token_string(a.get("priority")).unwrap_or_else(|| "0".into()));
        let pb = vb_val(&token_string(b.get("priority")).unwrap_or_else(|| "0".into()));
        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut merged: Option<Value> = None;
    for patch in sorted {
        if token_string(patch.get("id")).is_none() {
            continue; // "存在为空的 HMCL 分支项"
        }
        match &mut merged {
            None => merged = Some(patch.clone()),
            Some(current) => newton_merge(current, patch),
        }
    }
    let mut result = merged.ok_or("all HMCL patches are empty")?;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("id".to_string(), Value::String(instance_name.to_string()));
        obj.remove("inheritsFrom");
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// inheritsFrom resolution (ModMinecraft.vb JsonObject getter).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveOutcome {
    pub json: Value,
    /// The instance's adjusted inherit name (the `McInstance.InheritName`
    /// property): the chain link callers should follow next, after the
    /// LiteLoader / HMCL adjustments. Empty when the chain ends here.
    pub inherit_name: String,
    /// Warnings in the order the original logs them (merge failures are
    /// non-fatal there: the getter returns the partially merged object).
    pub warnings: Vec<String>,
}

/// Resolve a version JSON's full `inheritsFrom` chain. `resolver`
/// supplies the *raw* JSON of a parent version by name (in PCL the
/// parent is loaded from `versions/<name>/<name>.json`); None models
/// the parent's JSON failing to load, which the original catches and
/// logs, keeping the object merged so far.
///
/// Faithfulness notes (all verified against the VB source):
/// - Child wins scalar conflicts; `libraries` are rebuilt as
///   child-libraries followed by parent-libraries (the #5978 comment:
///   classpath order decides duplicate classes on Java 8-).
/// - Because the parent's *resolved* JSON still carries its own
///   `inheritsFrom` field (child properties win the merge, including
///   that field), a chain of depth 3+ merges the deepest ancestor an
///   extra time — its libraries appear twice in the result before the
///   library-list dedup hides it. The port reproduces this.
/// - A self-referencing parent (`parent.InheritName == inherit target`)
///   aborts that merge step with a "版本依赖项出现嵌套" warning.
/// - Chain links after each merge step follow the parent's *adjusted*
///   inherit name (`McInstance.InheritName`, ported as
///   [`inherit_name_of`]), not the merged JSON's raw `inheritsFrom` —
///   this is what lets an old LiteLoader parent (no `inheritsFrom`
///   field at all) still chain into its vanilla version. For ordinary
///   parents the two coincide, so the depth-3+ double-merge behaviour
///   above is unchanged. HMCL parents resolve to an empty inherit
///   name, as in the original.
/// - A recursion depth cap (64) replaces the original's unbounded
///   re-entry on cyclic chains (in practice a stack overflow there).
pub fn resolve_inheritance(
    instance_name: &str,
    raw: Value,
    resolver: &dyn Fn(&str) -> Option<Value>,
) -> ResolveOutcome {
    resolve_inner(instance_name, raw, resolver, 0)
}

fn resolve_inner(
    instance_name: &str,
    raw: Value,
    resolver: &dyn Fn(&str) -> Option<Value>,
    depth: usize,
) -> ResolveOutcome {
    let mut warnings: Vec<String> = Vec::new();
    // The instance's own JSON text and format, captured before any
    // conversion: `McInstance.InheritName` inspects the raw JsonText.
    // (The port re-serializes the parsed JSON; the substrings the
    // adjustment looks for live inside string tokens, so this is
    // equivalent for that purpose.)
    let raw_text = serde_json::to_string(&raw).unwrap_or_default();
    let hmcl = is_hmcl_format(&raw);
    let mut obj = raw;

    // HMCL conversion happens before inheritance handling.
    if hmcl {
        match merge_hmcl_patches(&obj, instance_name) {
            Ok(converted) => obj = converted,
            Err(e) => {
                warnings.push(format!("合并 HMCL 分支项失败：{e}"));
                let inherit_name = inherit_name_of(instance_name, &raw_text, &obj, hmcl);
                return ResolveOutcome {
                    json: obj,
                    inherit_name,
                    warnings,
                };
            }
        }
    }

    let mut inherit = inherits_from_of(&obj);
    if inherit == instance_name {
        warnings.push(format!("自引用的继承版本：{instance_name}"));
        let inherit_name = inherit_name_of(instance_name, &raw_text, &obj, hmcl);
        return ResolveOutcome {
            json: obj,
            inherit_name,
            warnings,
        };
    }

    while !inherit.is_empty() {
        if depth >= 64 {
            warnings.push("版本依赖项层级过深，已停止合并".to_string());
            break;
        }
        let Some(parent_raw) = resolver(&inherit) else {
            warnings.push(format!("合并版本依赖项 JSON 失败（{inherit}）"));
            break;
        };
        let parent = resolve_inner(&inherit, parent_raw, resolver, depth + 1);
        warnings.extend(parent.warnings);
        let parent_resolved = parent.json;
        // The next chain link is the parent's adjusted InheritName
        // (VB: `InstanceNameInheritFrom = InheritInstance.InheritName`),
        // which is also what the nesting check compares against.
        let parent_inherit = parent.inherit_name.clone();
        if parent_inherit == inherit {
            warnings.push(format!("版本依赖项出现嵌套：{inherit}"));
            break;
        }
        // Libraries: child first, then the parent's (already resolved).
        let current_libs = obj.get("libraries").and_then(Value::as_array).cloned();
        let parent_libs = parent_resolved.get("libraries").and_then(Value::as_array).cloned();
        let (Some(mut combined), Some(parent_lib_list)) = (current_libs, parent_libs) else {
            // The original dereferences both arrays unconditionally;
            // a missing one aborts the merge with a logged warning.
            warnings.push(format!("合并版本依赖项 JSON 失败（{inherit}）"));
            break;
        };
        combined.extend(parent_lib_list);
        let mut merged = parent_resolved;
        newton_merge(&mut merged, &obj);
        if let Some(map) = merged.as_object_mut() {
            map.insert("libraries".to_string(), Value::Array(combined));
        }
        obj = merged;
        inherit = parent_inherit;
    }

    let inherit_name = inherit_name_of(instance_name, &raw_text, &obj, hmcl);
    ResolveOutcome {
        json: obj,
        inherit_name,
        warnings,
    }
}

/// The adjusted inherit name of an instance — a port of the
/// `McInstance.InheritName` property getter (`ModMinecraft.vb`).
///
/// Starts from the resolved JSON's `inheritsFrom`. Very old LiteLoader
/// versions (e.g. 1.5.2) carry no `inheritsFrom` at all, so when the raw
/// JSON text mentions liteloader, does not mention logging, and the
/// detected vanilla name differs from the instance (folder) name, the
/// inherit name is replaced by the vanilla name — unless the JSON
/// provides a *different* jar, which means the jar was replaced and the
/// substitution is skipped. HMCL-format instances always report "".
///
/// `resolved_json` is the instance's inheritance-resolved JSON (the
/// original reads the merged `JsonObject` and builds `McVersion` from
/// it); the jar's embedded version.json is not available through the
/// resolver abstraction, so vanilla detection runs without it — that
/// input only matters when every other detection step has failed.
pub fn inherit_name_of(
    instance_name: &str,
    raw_json_text: &str,
    resolved_json: &Value,
    is_hmcl_format_json: bool,
) -> String {
    let mut name = inherits_from_of(resolved_json);
    if raw_json_text.contains("liteloader") && !raw_json_text.contains("logging") {
        let vanilla = get_vanilla_name(instance_name, resolved_json, None).name;
        if vanilla != instance_name {
            // `If(JsonObject("jar"), Version.VanillaName)`: a missing
            // jar entry defaults to the vanilla name itself.
            let jar = token_string(resolved_json.get("jar")).unwrap_or_else(|| vanilla.clone());
            if jar == vanilla {
                name = vanilla;
            }
        }
    }
    if is_hmcl_format_json {
        name = String::new();
    }
    name
}

/// The version JSON's `mainClass`, the field `McInstance.Check`
/// requires to exist.
pub fn main_class_of(json: &Value) -> Option<String> {
    token_string(json.get("mainClass"))
}

/// Whether the JSON is in the legacy format (`IsOldJson`): a non-empty
/// `minecraftArguments` string is present.
pub fn is_old_json(json: &Value) -> bool {
    matches!(json.get("minecraftArguments"), Some(Value::String(s)) if !s.is_empty())
}

// ---------------------------------------------------------------------------
// McJsonRuleCheck — rules evaluation, Windows-flavoured as in PCL.
// ---------------------------------------------------------------------------

/// Port of `ModMinecraft.McJsonRuleCheck`. `rules` is the raw `rules`
/// array of a library or argument entry; None means "no rules" and
/// passes. `os_version` is the string PCL reads from
/// `My.Computer.Info.OSVersion`, used only for `os.version` regexes.
///
/// PCL's simplifications versus the Mojang spec are kept: the platform
/// is assumed to be 64-bit Windows (an `arch` rule of `x86`, or one
/// containing `arm`, never matches), a present `is_demo_user` feature
/// never matches, and any feature key containing `quick_play` never
/// matches. Rules are applied in order, later rules overriding earlier
/// ones; with rules present but none matching, the result is false.
pub fn rule_check(rules: Option<&Value>, os_version: &str) -> bool {
    rule_check_for_os(rules, os_version, "windows")
}

/// [`rule_check`] generalized to a host OS key ("windows" / "linux" /
/// "osx", the Mojang rule vocabulary) for the cross-platform launch
/// path (PORT-PLAN §8): the named OS matches (with its `os.version`
/// regex applied to `os_version`), "unknown" always passes, any
/// other OS never matches. With `os_key == "windows"` this is
/// exactly PCL's original behavior.
pub fn rule_check_for_os(rules: Option<&Value>, os_version: &str, os_key: &str) -> bool {
    let Some(rules) = rules else {
        return true;
    };
    let Some(rule_list) = rules.as_array() else {
        return true;
    };
    let mut required = false;
    for rule in rule_list {
        let mut is_right_rule = true;
        if let Some(os) = rule.get("os") {
            if let Some(name) = token_string(os.get("name")) {
                match name.as_str() {
                    "unknown" => {}
                    _ if name == os_key => {
                        if let Some(version_pattern) = token_string(os.get("version")) {
                            let matches = regex::Regex::new(&version_pattern)
                                .map(|re| re.is_match(os_version))
                                .unwrap_or(false);
                            is_right_rule = is_right_rule && matches;
                        }
                    }
                    _ => {
                        is_right_rule = false;
                    }
                }
            }
            if let Some(arch) = token_string(os.get("arch")) {
                is_right_rule = is_right_rule && arch != "x86" && !arch.contains("arm");
            }
        }
        if let Some(features) = rule.get("features") {
            // is_demo_user present (non-null) -> not a match.
            let demo = features.get("is_demo_user");
            is_right_rule = is_right_rule && matches!(demo, None | Some(Value::Null));
            if let Some(obj) = features.as_object() {
                if obj.keys().any(|k| k.contains("quick_play")) {
                    is_right_rule = false;
                }
            }
        }
        let action = token_string(rule.get("action"));
        if action.as_deref() == Some("allow") {
            if is_right_rule {
                required = true;
            }
        } else if is_right_rule {
            required = false;
        }
    }
    required
}

// ---------------------------------------------------------------------------
// McLibGet / McLibEntry / McLibListGetWithJson.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibListError {
    LibrariesMissing,
    MalformedLibraryName(String),
    LibraryNotObject,
}

impl std::fmt::Display for LibListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibrariesMissing => write!(f, "version JSON has no libraries array"),
            Self::MalformedLibraryName(n) => write!(f, "malformed library name: {n}"),
            Self::LibraryNotObject => write!(f, "library entry is not an object"),
        }
    }
}
impl std::error::Error for LibListError {}

/// Port of `ModMinecraft.McLibGet`: the libraries-folder path for a
/// Maven coordinate. The folder part ignores a 4th segment (classifier)
/// while the file name keeps everything after the group id, per #7306.
pub fn mc_lib_get(original: &str, with_head: bool, mc_folder: &str) -> Result<String, LibListError> {
    let splited: Vec<&str> = original.split(':').collect();
    if splited.len() < 3 {
        return Err(LibListError::MalformedLibraryName(original.to_string()));
    }
    Ok(format!(
        "{}{}\\{}\\{}\\{}.jar",
        if with_head { format!("{mc_folder}libraries\\") } else { String::new() },
        splited[0].replace('.', "\\"),
        splited[1],
        splited[2],
        splited[1..].join("-")
    ))
}

/// One entry of PCL's `McLibEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibEntry {
    /// The JSON `name` coordinate; may be None for the main jar entry
    /// PCL appends itself.
    pub original_name: Option<String>,
    pub local_path: String,
    pub size: i64,
    pub is_natives: bool,
    pub sha1: Option<String>,
    /// The JSON-provided URL; an empty/whitespace URL becomes None,
    /// matching the original's property setter.
    pub url: Option<String>,
}

impl LibEntry {
    pub fn new(
        original_name: Option<String>,
        local_path: String,
        size: i64,
        is_natives: bool,
        sha1: Option<String>,
        url: Option<String>,
    ) -> LibEntry {
        let url = url.filter(|u| !u.trim().is_empty());
        LibEntry { original_name, local_path, size, is_natives, sha1, url }
    }

    /// `McLibEntry.Name`: the coordinate with the version segment
    /// (index 2) removed; classifier segments are kept.
    pub fn name(&self) -> Option<String> {
        let original = self.original_name.as_ref()?;
        let mut parts: Vec<&str> = original.split(':').collect();
        if parts.len() < 3 {
            return None;
        }
        parts.remove(2);
        Some(parts.join(":"))
    }
}

/// The version directory of an entry, as `McLibListGetWithJson`'s dedup
/// derives it: the folder name above the jar file.
fn entry_version(entry: &LibEntry) -> String {
    pathutil::get_last_part(&pathutil::remove_last_part(&entry.local_path))
}

/// Port of `ModMinecraft.McLibListGetWithJson` on an already-resolved
/// version JSON. `os_version` feeds `rule_check`; `is_64bit_os`
/// replaces `${arch}` in natives paths. Per-entry processing failures
/// fall back to a bare entry exactly where the original's Try/Catch
/// does; structurally malformed input errors out as a whole, also as
/// in the original.
pub fn lib_list_from_json(
    json: &Value,
    mc_folder: &str,
    keep_same_name_different_version: bool,
    is_64bit_os: bool,
    os_version: &str,
) -> Result<Vec<LibEntry>, LibListError> {
    lib_list_from_json_os(
        json,
        mc_folder,
        keep_same_name_different_version,
        is_64bit_os,
        os_version,
        "windows",
    )
}

/// [`lib_list_from_json`] for a non-Windows host (PORT-PLAN §8):
/// library rules and the natives classifier are resolved against
/// `os_key` ("linux" / "osx") instead of PCL's native "windows".
/// PCL itself only ever runs on Windows, so this is the port's
/// cross-platform adaptation point; everything else (paths, dedup,
/// fallbacks) is unchanged.
pub fn lib_list_from_json_os(
    json: &Value,
    mc_folder: &str,
    keep_same_name_different_version: bool,
    is_64bit_os: bool,
    os_version: &str,
    os_key: &str,
) -> Result<Vec<LibEntry>, LibListError> {
    let libraries = json
        .get("libraries")
        .and_then(Value::as_array)
        .ok_or(LibListError::LibrariesMissing)?;

    let mut basic: Vec<LibEntry> = Vec::new();
    for library in libraries {
        let Some(lib_obj) = library.as_object() else {
            return Err(LibListError::LibraryNotObject);
        };
        // Strip null-valued properties (BakaXL serializes missing
        // fields as null, see #409 in the original).
        let mut cleaned = serde_json::Map::new();
        for (k, v) in lib_obj {
            if !v.is_null() {
                cleaned.insert(k.clone(), v.clone());
            }
        }
        let library = Value::Object(cleaned);

        if !rule_check_for_os(library.get("rules"), os_version, os_key) {
            continue;
        }
        let name = token_string(library.get("name"))
            .ok_or_else(|| LibListError::MalformedLibraryName(String::new()))?;
        let root_url = match token_string(library.get("url")) {
            Some(url) => Some(format!(
                "{}{}",
                url,
                mc_lib_get(&name, false, mc_folder)?.replace('\\', "/")
            )),
            None => None,
        };

        let natives = library.get("natives");
        if natives.is_none() {
            // Plain library.
            let local_path = mc_lib_get(&name, true, mc_folder)?;
            let artifact = library
                .get("downloads")
                .and_then(|d| d.get("artifact"))
                .filter(|a| !a.is_null());
            let entry = match artifact {
                Some(artifact) => {
                    let size_token = artifact.get("size");
                    match size_token {
                        Some(size_value) => {
                            let size = crate::vrange::vb_val_int(
                                &token_string(Some(size_value)).unwrap_or_default(),
                            );
                            let local = match token_string(artifact.get("path")) {
                                Some(p) => format!(
                                    "{mc_folder}libraries\\{}",
                                    p.replace('/', "\\")
                                ),
                                None => local_path.clone(),
                            };
                            LibEntry::new(
                                Some(name.clone()),
                                local,
                                size,
                                false,
                                token_string(artifact.get("sha1")),
                                token_string(artifact.get("url")).or_else(|| root_url.clone()),
                            )
                        }
                        // A missing size throws in the original and
                        // lands in the fallback entry below.
                        None => LibEntry::new(
                            Some(name.clone()),
                            local_path.clone(),
                            0,
                            false,
                            None,
                            root_url.clone(),
                        ),
                    }
                }
                None => LibEntry::new(
                    Some(name.clone()),
                    local_path.clone(),
                    0,
                    false,
                    None,
                    root_url.clone(),
                ),
            };
            basic.push(entry);
        } else if let Some(native_name) = natives.and_then(|n| token_string(n.get(os_key))) {
            // Host-OS natives library ("windows" in the original).
            let fallback_local = mc_lib_get(&name, true, mc_folder)?
                .replace(".jar", &format!("-{native_name}.jar"))
                .replace("${arch}", if is_64bit_os { "64" } else { "32" });
            let classifier = library
                .get("downloads")
                .and_then(|d| d.get("classifiers"))
                .and_then(|c| c.get(&native_name))
                .filter(|c| !c.is_null());
            let entry = match classifier {
                Some(data) => {
                    let sha1 = data.get("sha1");
                    let size_token = data.get("size");
                    match (sha1, size_token) {
                        (Some(sha1_value), Some(size_value)) => {
                            let local = match token_string(data.get("path")) {
                                Some(p) => format!(
                                    "{mc_folder}libraries\\{}",
                                    p.replace('/', "\\")
                                ),
                                None => fallback_local.clone(),
                            };
                            LibEntry::new(
                                Some(name.clone()),
                                local,
                                crate::vrange::vb_val_int(
                                    &token_string(Some(size_value)).unwrap_or_default(),
                                ),
                                true,
                                // The original calls .ToString directly:
                                // a JSON-null sha1 becomes "".
                                Some(match sha1_value {
                                    Value::Null => String::new(),
                                    other => token_string(Some(other)).unwrap_or_default(),
                                }),
                                token_string(data.get("url")).or_else(|| root_url.clone()),
                            )
                        }
                        _ => LibEntry::new(
                            Some(name.clone()),
                            fallback_local.clone(),
                            0,
                            true,
                            None,
                            root_url.clone(),
                        ),
                    }
                }
                None => LibEntry::new(
                    Some(name.clone()),
                    fallback_local.clone(),
                    0,
                    true,
                    None,
                    root_url.clone(),
                ),
            };
            basic.push(entry);
        }
        // A natives block without a host-OS entry contributes nothing.
    }

    // Dedup by (name, is_natives): a higher version replaces a lower
    // one in place; with `keep_same_name_different_version`, differing
    // versions are both kept (appended).
    let mut result: Vec<LibEntry> = Vec::new();
    for entry in basic {
        let entry_name = entry
            .name()
            .ok_or_else(|| LibListError::MalformedLibraryName(entry.original_name.clone().unwrap_or_default()))?;
        let key = (entry_name, entry.is_natives);
        let existing_idx = result
            .iter()
            .position(|e| e.name() == Some(key.0.clone()) && e.is_natives == key.1);
        match existing_idx {
            None => result.push(entry),
            Some(idx) => {
                let new_version = entry_version(&entry);
                let old_version = entry_version(&result[idx]);
                if new_version != old_version && keep_same_name_different_version {
                    result.push(entry);
                } else if crate::vrange::compare_version_ge(&new_version, &old_version) {
                    result[idx] = entry;
                }
            }
        }
    }
    Ok(result)
}

/// Assemble `${classpath}` the way `McLaunchArgumentsReplace` does:
/// non-natives entries in list order (the caller appends the main jar
/// entry last, as `McLibListGet(Instance, True)` does), the OptiFine
/// entry pulled out and re-inserted second-to-last, every path passed
/// through the short-path transform, joined with `;`.
pub fn build_classpath(entries: &[LibEntry], short_path: &dyn Fn(&str) -> String) -> String {
    let mut cps: Vec<String> = Vec::new();
    let mut optifine: Option<String> = None;
    for entry in entries {
        if entry.is_natives {
            continue;
        }
        if entry.original_name.as_deref() == Some("optifine:OptiFine") {
            optifine = Some(entry.local_path.clone());
        } else {
            cps.push(entry.local_path.clone());
        }
    }
    if let Some(optifine_path) = optifine {
        if cps.len() >= 2 {
            let at = cps.len() - 2;
            cps.insert(at, optifine_path);
        } else {
            // The original throws (Insert at a negative index); with
            // fewer than two entries there is no second-to-last slot.
            cps.push(optifine_path);
        }
    }
    cps.iter()
        .map(|c| short_path(c))
        .collect::<Vec<_>>()
        .join(";")
}

/// Summary of one installed instance found by [`scan_instances`].
#[derive(Debug, Clone)]
pub struct InstanceSummary {
    pub folder_name: String,
    pub json_id: Option<String>,
    pub inherits_from: Option<String>,
    pub main_class: String,
    pub library_count: usize,
    pub warnings: Vec<String>,
}

/// Scan a `versions` directory: every subdirectory holding a
/// `<folder>.json` is an instance; inheritance is resolved against
/// its siblings (PCL reads the parent JSON from the same versions
/// folder). Unreadable or malformed entries are skipped with a
/// warning rather than aborting the scan.
pub fn scan_instances(versions_dir: &std::path::Path) -> Vec<InstanceSummary> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(versions_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if entry.path().join(format!("{name}.json")).is_file() {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    let load = |name: &str| -> Option<Value> {
        let text = std::fs::read_to_string(
            versions_dir.join(name).join(format!("{name}.json")),
        )
        .ok()?;
        serde_json::from_str(&text).ok()
    };
    let resolver = |name: &str| load(name);
    names
        .iter()
        .map(|name| {
            let Some(json) = load(name) else {
                return InstanceSummary {
                    folder_name: name.clone(),
                    json_id: None,
                    inherits_from: None,
                    main_class: String::new(),
                    library_count: 0,
                    warnings: vec!["版本 JSON 无法读取或解析".to_string()],
                };
            };
            let json_id = token_string(json.get("id"));
            let inherits_from = token_string(json.get("inheritsFrom"));
            let outcome = resolve_inheritance(name, json, &resolver);
            let mut warnings = outcome.warnings.clone();
            let library_count = match lib_list_from_json(
                &outcome.json,
                versions_dir
                    .parent()
                    .map(|p| p.to_string_lossy().replace('/', "\\"))
                    .unwrap_or_default()
                    .trim_end_matches('\\'),
                false,
                true,
                "Microsoft Windows NT 10.0.0.0",
            ) {
                Ok(libs) => libs.len(),
                Err(e) => {
                    warnings.push(format!("依赖库列表解析失败：{e}"));
                    0
                }
            };
            InstanceSummary {
                folder_name: name.clone(),
                json_id,
                inherits_from,
                main_class: main_class_of(&outcome.json).unwrap_or_default(),
                library_count,
                warnings,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scan_instances_finds_and_resolves() {
        let base = std::env::temp_dir().join(format!("pcl-scan-test-{}", std::process::id()));
        let versions = base.join("versions");
        let parent_dir = versions.join("1.20.1");
        let child_dir = versions.join("fabric-1.20.1");
        std::fs::create_dir_all(&parent_dir).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            parent_dir.join("1.20.1.json"),
            r#"{"id":"1.20.1","mainClass":"net.minecraft.client.main.Main","libraries":[{"name":"a:b:1"}]}"#,
        )
        .unwrap();
        std::fs::write(
            child_dir.join("fabric-1.20.1.json"),
            r#"{"id":"fabric-1.20.1","inheritsFrom":"1.20.1","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[{"name":"c:d:2"}]}"#,
        )
        .unwrap();
        let found = scan_instances(&versions);
        std::fs::remove_dir_all(&base).ok();
        assert_eq!(found.len(), 2);
        let child = found.iter().find(|f| f.folder_name == "fabric-1.20.1").unwrap();
        assert_eq!(child.main_class, "net.fabricmc.loader.impl.launch.knot.KnotClient");
        assert_eq!(child.library_count, 2); // own lib + inherited lib
        assert!(child.warnings.is_empty());
    }

    #[test]
    fn newton_merge_semantics() {
        let mut target = json!({"a": 1, "obj": {"x": 1, "y": 2}, "arr": [1, 2], "keep": "p"});
        let source = json!({"a": 2, "obj": {"y": 3, "z": 4}, "arr": [3], "new": null});
        newton_merge(&mut target, &source);
        assert_eq!(
            target,
            json!({"a": 2, "obj": {"x": 1, "y": 3, "z": 4}, "arr": [1, 2, 3], "keep": "p", "new": null})
        );
    }

    #[test]
    fn hmcl_patches_merge_by_priority() {
        let json = json!({
            "patches": [
                {"id": "game", "priority": 10, "mainClass": "GameMain", "libraries": [{"name": "a:b:1"}]},
                {"id": "forge", "priority": 20, "mainClass": "ForgeMain", "libraries": [{"name": "c:d:2"}]},
                {"priority": 1, "mainClass": "NoId"}
            ],
            "id": "hmcl-instance"
        });
        let merged = merge_hmcl_patches(&json, "my-instance").unwrap();
        assert_eq!(merged["id"], "my-instance");
        assert_eq!(merged["mainClass"], "ForgeMain");
        // Arrays concatenate in merge order (game first, then forge).
        assert_eq!(merged["libraries"].as_array().unwrap().len(), 2);
        assert!(merged.get("inheritsFrom").is_none());
    }

    fn chain_resolver(map: &[(&str, Value)]) -> impl Fn(&str) -> Option<Value> {
        let owned: Vec<(String, Value)> =
            map.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        move |name: &str| owned.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn inheritance_child_wins_and_libraries_child_first() {
        let parent = json!({
            "id": "1.20.1", "type": "release", "mainClass": "ParentMain",
            "libraries": [{"name": "p:lib:1"}],
            "arguments": {"game": ["--parent"], "jvm": ["-Xparent"]},
            "minecraftArguments": "parent-args",
            "assets": "5"
        });
        let child = json!({
            "id": "fabric-1.20.1", "inheritsFrom": "1.20.1", "type": "release",
            "mainClass": "ChildMain",
            "libraries": [{"name": "c:lib:2"}],
            "arguments": {"game": ["--child"]}
        });
        let resolver = chain_resolver(&[("1.20.1", parent)]);
        let out = resolve_inheritance("fabric-1.20.1", child, &resolver);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let json = &out.json;
        assert_eq!(json["mainClass"], "ChildMain");
        // String the child lacks comes from the parent.
        assert_eq!(json["minecraftArguments"], "parent-args");
        assert_eq!(json["assets"], "5");
        // Libraries: child's first, then parent's.
        let libs = json["libraries"].as_array().unwrap();
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[0]["name"], "c:lib:2");
        assert_eq!(libs[1]["name"], "p:lib:1");
        // Arguments objects deep-merge; game arrays concatenate
        // parent-first under Newtonsoft semantics.
        let game = json["arguments"]["game"].as_array().unwrap();
        assert_eq!(game, &vec![json!("--parent"), json!("--child")]);
        assert_eq!(json["arguments"]["jvm"], json!(["-Xparent"]));
    }

    #[test]
    fn inheritance_deep_chain_duplicates_deepest_libraries() {
        // A -> B -> C: PCL's algorithm merges C twice (see module docs).
        let c = json!({"id": "C", "mainClass": "CMain", "libraries": [{"name": "c:lib:1"}]});
        let b = json!({"id": "B", "inheritsFrom": "C", "libraries": [{"name": "b:lib:1"}]});
        let a = json!({"id": "A", "inheritsFrom": "B", "libraries": [{"name": "a:lib:1"}]});
        let resolver = chain_resolver(&[("B", b), ("C", c)]);
        let out = resolve_inheritance("A", a, &resolver);
        let names: Vec<&str> = out.json["libraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a:lib:1", "b:lib:1", "c:lib:1", "c:lib:1"]);
        assert_eq!(out.json["mainClass"], "CMain");
    }

    #[test]
    fn inheritance_self_and_missing_parent() {
        let self_ref = json!({"id": "X", "inheritsFrom": "X", "libraries": []});
        let resolver = chain_resolver(&[]);
        let out = resolve_inheritance("X", self_ref, &resolver);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("自引用"));

        let orphan = json!({"id": "Y", "inheritsFrom": "ghost", "libraries": []});
        let out = resolve_inheritance("Y", orphan, &resolver);
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.json["id"], "Y");
    }

    #[test]
    fn rule_check_windows_semantics() {
        // No rules -> pass.
        assert!(rule_check(None, "Microsoft Windows NT 10.0.22631.0"));
        let allow_win = json!([{"action": "allow", "os": {"name": "windows"}}]);
        assert!(rule_check(Some(&allow_win), "Microsoft Windows NT 10.0.22631.0"));
        let allow_osx = json!([{"action": "allow", "os": {"name": "osx"}}]);
        assert!(!rule_check(Some(&allow_osx), "Microsoft Windows NT 10.0.22631.0"));
        // Version regex: PCL matches it against the *full* OS version
        // string ("Microsoft Windows NT 10.0..."), so Mojang's usual
        // anchored "^10\." pattern never matches there — this disallow
        // is inert upstream, and the port keeps that behavior.
        let both = json!([
            {"action": "allow"},
            {"action": "disallow", "os": {"name": "windows", "version": "^10\\."}}
        ]);
        assert!(rule_check(Some(&both), "Microsoft Windows NT 10.0.22631.0"));
        // The unanchored form does match and the later disallow wins.
        let both_unanchored = json!([
            {"action": "allow"},
            {"action": "disallow", "os": {"name": "windows", "version": "10\\."}}
        ]);
        assert!(!rule_check(Some(&both_unanchored), "Microsoft Windows NT 10.0.22631.0"));
        assert!(rule_check(Some(&both_unanchored), "Microsoft Windows NT 6.3.9600.0"));
        // x86 arch rules never match on PCL's assumed 64-bit platform.
        let x86 = json!([{"action": "allow", "os": {"name": "windows", "arch": "x86"}}]);
        assert!(!rule_check(Some(&x86), "Microsoft Windows NT 10.0.22631.0"));
        // Feature rules: is_demo_user / quick_play never match.
        let demo = json!([{"action": "allow", "features": {"is_demo_user": true}}]);
        assert!(!rule_check(Some(&demo), "Microsoft Windows NT 10.0.22631.0"));
        let qp = json!([{"action": "allow", "features": {"is_quick_play_singleplayer": true}}]);
        assert!(!rule_check(Some(&qp), "Microsoft Windows NT 10.0.22631.0"));
    }

    #[test]
    fn mc_lib_get_paths() {
        assert_eq!(
            mc_lib_get("org.lwjgl:lwjgl:3.3.2", true, r"C:\MC\").unwrap(),
            r"C:\MC\libraries\org\lwjgl\lwjgl\3.3.2\lwjgl-3.3.2.jar"
        );
        // 4-segment coordinate: classifier kept in the file name only.
        assert_eq!(
            mc_lib_get("net.minecraftforge:forge:1.20.1-47.1.0:client", true, r"C:\MC\").unwrap(),
            r"C:\MC\libraries\net\minecraftforge\forge\1.20.1-47.1.0\forge-1.20.1-47.1.0-client.jar"
        );
        assert!(mc_lib_get("only:two", true, r"C:\MC\").is_err());
    }

    #[test]
    fn lib_list_builds_and_dedups() {
        let json = json!({
            "libraries": [
                {"name": "com.google.guava:guava:31.1-jre",
                 "downloads": {"artifact": {"path": "com/google/guava/guava/31.1-jre/guava-31.1-jre.jar",
                     "sha1": "abc", "size": 100, "url": "https://example.invalid/guava.jar"}}},
                {"name": "com.google.guava:guava:32.0.0-jre"},
                {"name": "org.lwjgl:lwjgl:3.3.2",
                 "natives": {"windows": "natives-windows"},
                 "downloads": {"classifiers": {"natives-windows": {"path": "org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2-natives-windows.jar", "sha1": "def", "size": 50, "url": "https://example.invalid/n.jar"}}}},
                {"name": "ca.weblite:java-objc-bridge:1.1",
                 "rules": [{"action": "allow", "os": {"name": "osx"}}]},
                {"name": "no.windows:natives:1.0", "natives": {"linux": "natives-linux"}}
            ]
        });
        let libs = lib_list_from_json(&json, r"C:\MC\", false, true, "Microsoft Windows NT 10.0.22631.0")
            .unwrap();
        // guava dedup keeps the higher version; osx-only and
        // linux-natives entries are gone.
        assert_eq!(libs.len(), 2);
        let guava = &libs[0];
        assert_eq!(guava.original_name.as_deref(), Some("com.google.guava:guava:32.0.0-jre"));
        assert_eq!(guava.url, None);
        let natives = &libs[1];
        assert!(natives.is_natives);
        assert_eq!(
            natives.local_path,
            r"C:\MC\libraries\org\lwjgl\lwjgl\3.3.2\lwjgl-3.3.2-natives-windows.jar"
        );
        assert_eq!(natives.sha1.as_deref(), Some("def"));
    }

    #[test]
    fn classpath_skips_natives_and_places_optifine() {
        let mk = |name: &str, path: &str, natives: bool| LibEntry {
            original_name: Some(name.to_string()),
            local_path: path.to_string(),
            size: 0,
            is_natives: natives,
            sha1: None,
            url: None,
        };
        let entries = vec![
            mk("a:a:1", r"C:\MC\libraries\a.jar", false),
            mk("optifine:OptiFine", r"C:\MC\libraries\of.jar", false),
            mk("b:b:1", r"C:\MC\libraries\b-natives.jar", true),
            mk("c:c:1", r"C:\MC\libraries\c.jar", false),
            mk("client", r"C:\MC\versions\1.20.1\1.20.1.jar", false),
        ];
        let mut with_main = entries.clone();
        with_main[4].original_name = None;
        let cp = build_classpath(&with_main, &|p| p.to_string());
        assert_eq!(
            cp,
            r"C:\MC\libraries\a.jar;C:\MC\libraries\of.jar;C:\MC\libraries\c.jar;C:\MC\versions\1.20.1\1.20.1.jar"
        );
    }

    /// An old-style LiteLoader version JSON: a liteloader library, the
    /// launchwrapper main class, legacy arguments — and crucially no
    /// `inheritsFrom` and no `logging` section.
    fn liteloader_json() -> Value {
        json!({
            "id": "liteloader-1.5.2",
            "type": "release",
            "mainClass": "net.minecraft.launchwrapper.Launch",
            "minecraftArguments": "--tweakClass com.mumfrey.liteloader.core.LiteLoaderTweaker",
            "libraries": [{"name": "com.mumfrey:liteloader:1.5.2_01"}]
        })
    }

    #[test]
    fn inherit_name_liteloader_adjustment() {
        let ll = liteloader_json();
        let raw_text = serde_json::to_string(&ll).unwrap();
        // The vanilla name (1.5.2, detected from the library coordinate
        // / id) replaces the empty inherit name.
        assert_eq!(
            inherit_name_of("liteloader-1.5.2", &raw_text, &ll, false),
            "1.5.2"
        );
        // The same JSON as its own outcome: ResolveOutcome carries it.
        let out = resolve_inheritance("liteloader-1.5.2", ll.clone(), &|_| None);
        assert_eq!(out.inherit_name, "1.5.2");
        // A `logging` section (newer JSON shape) disables the adjustment.
        let mut with_logging = ll.clone();
        with_logging["logging"] = json!({});
        let text = serde_json::to_string(&with_logging).unwrap();
        assert_eq!(
            inherit_name_of("liteloader-1.5.2", &text, &with_logging, false),
            ""
        );
        // A replaced jar also disables it: with the vanilla name
        // pinned by clientVersion (an earlier detection step than the
        // jar entry), a jar that names something else means the jar
        // was swapped, and the replacement is skipped.
        let mut with_jar = ll.clone();
        with_jar["clientVersion"] = json!("1.5.2");
        with_jar["jar"] = json!("custom-client");
        let text = serde_json::to_string(&with_jar).unwrap();
        assert_eq!(
            inherit_name_of("liteloader-1.5.2", &text, &with_jar, false),
            ""
        );
        // Instance name equal to the vanilla name: nothing to adjust.
        assert_eq!(inherit_name_of("1.5.2", &raw_text, &ll, false), "");
        // HMCL format always reports "".
        assert_eq!(inherit_name_of("liteloader-1.5.2", &raw_text, &ll, true), "");
    }

    #[test]
    fn inheritance_follows_liteloader_parent_into_vanilla() {
        // Grandchild -> LiteLoader (no inheritsFrom) -> vanilla 1.5.2:
        // the chain must follow the parent's adjusted InheritName,
        // exactly like the VB merge loop does.
        let vanilla = json!({
            "id": "1.5.2", "type": "release",
            "mainClass": "net.minecraft.client.Minecraft",
            "assets": "legacy",
            "libraries": [{"name": "v:lib:1"}]
        });
        let child = json!({
            "id": "child", "inheritsFrom": "liteloader-1.5.2",
            "mainClass": "ChildMain",
            "libraries": [{"name": "c:lib:1"}]
        });
        let resolver = chain_resolver(&[
            ("liteloader-1.5.2", liteloader_json()),
            ("1.5.2", vanilla),
        ]);
        let out = resolve_inheritance("child", child, &resolver);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let names: Vec<&str> = out.json["libraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["c:lib:1", "com.mumfrey:liteloader:1.5.2_01", "v:lib:1"]
        );
        // Child wins mainClass; the vanilla-only field comes through.
        assert_eq!(out.json["mainClass"], "ChildMain");
        assert_eq!(out.json["assets"], "legacy");
    }
}


#[cfg(test)]
mod os_variant_tests {
    use super::*;
    use serde_json::json;

    fn natives_json() -> Value {
        json!({
            "id": "1.20.1",
            "libraries": [
                {
                    "name": "org.lwjgl:lwjgl:3.3.2",
                    "natives": {"linux": "natives-linux", "windows": "natives-windows"},
                    "downloads": {"classifiers": {
                        "natives-linux": {
                            "path": "org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2-natives-linux.jar",
                            "sha1": "aaaa", "size": 50, "url": "https://example.invalid/linux.jar"
                        },
                        "natives-windows": {
                            "path": "org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2-natives-windows.jar",
                            "sha1": "bbbb", "size": 60, "url": "https://example.invalid/win.jar"
                        }
                    }}
                },
                {
                    "name": "com.example:linuxonly:1",
                    "rules": [{"action": "allow", "os": {"name": "linux"}}],
                    "downloads": {"artifact": {
                        "path": "com/example/linuxonly/1/linuxonly-1.jar",
                        "sha1": "cccc", "size": 10, "url": "https://example.invalid/l.jar"
                    }}
                }
            ]
        })
    }

    #[test]
    fn linux_host_gets_linux_natives_and_rules() {
        let json = natives_json();
        let entries =
            lib_list_from_json_os(&json, r"C:\MC\", false, true, "", "linux").unwrap();
        let natives: Vec<&LibEntry> = entries.iter().filter(|e| e.is_natives).collect();
        assert_eq!(natives.len(), 1);
        assert!(natives[0].local_path.contains("natives-linux"));
        assert!(
            entries
                .iter()
                .any(|e| e.original_name.as_deref() == Some("com.example:linuxonly:1")),
            "the linux-only rule passes on a linux host"
        );

        // The original Windows flavor: windows natives, and the
        // linux-only library is excluded.
        let entries = lib_list_from_json(&json, r"C:\MC\", false, true, "").unwrap();
        let natives: Vec<&LibEntry> = entries.iter().filter(|e| e.is_natives).collect();
        assert_eq!(natives.len(), 1);
        assert!(natives[0].local_path.contains("natives-windows"));
        assert!(
            !entries
                .iter()
                .any(|e| e.original_name.as_deref() == Some("com.example:linuxonly:1")),
            "the linux-only rule fails under PCL's Windows semantics"
        );
    }
}
