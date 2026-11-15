//! Launch argument construction — the pure-logic half of
//! `Plain Craft Launcher 2/Modules/Minecraft/ModLaunch.vb`
//! (`McLaunchArgumentsJVM`, `McLaunchArgumentsGame`,
//! `McLaunchArgumentsReplace`, `McLaunchArgumentMain`,
//! `SplitJavaArguments`, `DeduplicateJavaArguments`) plus
//! `ModMain.vb`'s `ArgumentReplace`.
//!
//! Process spawning, file extraction (JavaWrapper/LUA jars), the
//! authlib-injector prefetch request and UI hints stay with the
//! caller: everything here is deterministic string construction over
//! already-resolved inputs (settings and instance state arrive as
//! plain structs).

use crate::instance::{rule_check, token_string};
use crate::vrange::{vb_val, JavaVersion};
use serde_json::Value;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// SplitJavaArguments / DeduplicateJavaArguments.
// ---------------------------------------------------------------------------

/// MeloongCore `ReplaceLineEndings(" ", mergeMultiple)`.
fn replace_line_endings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' || c == '\n' {
            out.push(' ');
            while matches!(it.peek(), Some('\r' | '\n')) {
                it.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Port of PCL's `SplitJavaArguments`: split on spaces outside double
/// quotes; `\"` is an escaped quote that does not toggle quote state
/// (and is kept verbatim); the quote characters themselves are kept.
pub fn split_java_arguments(arg: &str) -> Vec<String> {
    let arg = replace_line_endings(arg);
    let arg = arg.trim();
    let mut result = Vec::new();
    if arg.is_empty() {
        return result;
    }
    let chars: Vec<char> = arg.chars().collect();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1] == '"' {
            current.push('\\');
            current.push('"');
            i += 2;
            continue;
        }
        if c == '"' {
            in_quotes = !in_quotes;
            current.push(c);
        } else if c == ' ' && !in_quotes {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
        i += 1;
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn after_first(s: &str, needle: &str) -> String {
    match s.find(needle) {
        Some(idx) => s[idx + needle.len()..].to_string(),
        None => s.to_string(),
    }
}

/// Port of PCL's `DeduplicateJavaArguments`. An argument starting with
/// `-` followed by a non-flag value forms a key/value pair (a following
/// `-50` counts as a value because `Val("50") != 0`, while `-x` counts
/// as another flag). Duplicate singles are dropped. For pairs: in JVM
/// mode (or for `--tweakClass`) an identical pair is dropped but a
/// conflicting pair is *kept twice*; in game mode a later pair
/// overwrites the earlier value in place.
pub fn deduplicate_java_arguments(args: Vec<String>, is_jvm_args: bool) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut i = 0usize;
    'outer: while i < args.len() {
        let key = args[i].clone();
        let is_pair = key.starts_with('-')
            && i + 1 < args.len()
            && !(args[i + 1].starts_with('-')
                && vb_val(&after_first(&args[i + 1], "-")) == 0.0);
        if !is_pair {
            i += 1;
            if result.contains(&key) {
                continue 'outer;
            }
            result.push(key);
        } else {
            let value = args[i + 1].clone();
            i += 2;
            let mut j: isize = 0;
            let count = result.len() as isize;
            while j <= count - 2 {
                let ju = j as usize;
                if result[ju] != key {
                    j += 1;
                    continue;
                }
                if !is_jvm_args && key != "--tweakClass" {
                    result[ju + 1] = value.clone();
                    continue 'outer;
                }
                if result[ju + 1] == value {
                    continue 'outer;
                }
                j += 1;
            }
            result.push(key);
            result.push(value);
        }
    }
    result
}

/// PCL's quoting rule for argument values containing spaces (used by
/// the JVM post-pass and the game-argument appender): wrap in double
/// quotes, escaping inner quotes, unless the value already starts or
/// ends with a quote.
pub fn quote_arg(a: &str) -> String {
    if a.contains(' ') && !a.starts_with('"') && !a.ends_with('"') {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_string()
    }
}

/// Evaluate a Mojang `arguments.game` / `arguments.jvm` entry list:
/// plain strings pass through; objects are included when their `rules`
/// pass and contribute their `value` (string or string array).
/// Non-string scalars in a value array render as text, as JToken
/// stringification does in the original.
pub fn eval_argument_entries(entries: &[Value], os_version: &str) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            Value::String(s) => out.push(s.clone()),
            Value::Object(obj) => {
                if let Some(rules) = obj.get("rules") {
                    if !rule_check(Some(rules), os_version) {
                        continue;
                    }
                }
                let Some(value) = obj.get("value") else {
                    continue;
                };
                match value {
                    Value::String(s) => out.push(s.clone()),
                    Value::Array(items) => {
                        for item in items {
                            if let Some(s) = token_string(Some(item)) {
                                out.push(s);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    out
}

/// One level of the instance chain as PCL's launch walk visits it: the
/// level's *resolved* JSON (its own `JsonObject`) and its
/// `InheritName` ("" ends the walk after that level).
pub struct ChainLevel<'a> {
    pub json: &'a Value,
    pub inherit_name: &'a str,
}

fn arguments_array<'a>(json: &'a Value, section: &str) -> Option<&'a Vec<Value>> {
    json.get("arguments")
        .and_then(|a| a.get(section))
        .and_then(Value::as_array)
}

// ---------------------------------------------------------------------------
// McLaunchArgumentsJVM.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// Authlib-Injector login needs the server's prefetch response,
    /// which requires a network request the pure layer cannot make.
    AuthPrefetchMissing,
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthPrefetchMissing => {
                write!(f, "无法连接到第三方登录服务器（缺少 prefetch 响应）")
            }
        }
    }
}
impl std::error::Error for LaunchError {}

#[derive(Debug, Clone)]
pub enum JvmLogin {
    None,
    /// 统一通行证: the nide server id from the version settings.
    Nide { server_id: String },
    /// Authlib-Injector: server URL plus the prefetched response body
    /// (fetched by the caller; base64-encoded here as in the original).
    Auth { server: String, prefetched_response: Option<String> },
}

#[derive(Debug, Clone)]
pub struct JvmInput {
    pub os_version: String,
    /// Custom JVM arguments from settings, already run through
    /// `argument_replace` by the caller ("" when unset).
    pub custom_arg: String,
    pub login: JvmLogin,
    /// The selected Java (major + full version + probed encodings).
    pub java_major: i64,
    pub java_version: JavaVersion,
    pub disable_jlw_global: bool,
    pub disable_jlw_instance: bool,
    /// `IsGBKEncoding` from the original (current encoding is GBK).
    pub is_gbk_encoding: bool,
    /// Path of the extracted JavaWrapper.jar. The original extracts it
    /// via a resource copy; None skips the trailing `-jar` pair.
    pub jlw_jar_path: Option<String>,
    /// Whether the LUA agent applies (original: lwjgl 3.4.1 present in
    /// the library list, agent not disabled).
    pub use_lua: bool,
    pub lua_jar_path: Option<String>,
    /// `-Xmx` megabytes (already `floor(GetRam * 1024)`).
    pub ram_mb: i64,
    /// `LaunchAdvanceGC` setting.
    pub gc_global: i32,
    /// Raw `VersionAdvanceGC` value; values > 0 replace the global as
    /// `value - 1`.
    pub gc_instance_override: Option<i32>,
    /// `Environment.OSVersion.Version` parts for the ZGC OS check.
    pub os_major: u32,
    pub os_build: u32,
    pub mode_debug: bool,
    /// Probed encodings of the selected Java (already lowercased by
    /// the probe, as in the original).
    pub file_encoding: Option<String>,
    pub native_encoding: Option<String>,
    /// Web name of the fallback (current-culture ANSI) encoding, used
    /// when the probed name is unrecognized.
    pub fallback_encoding_name: String,
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

const GC_ARG_PATTERN: &str =
    r"( )*-XX:[+-]?(Use\w+GC|ZGenerational|UseCompactObjectHeaders|G1\w+Percent|G1\w+Size|(Max|Min)(GCPauseMillis|HeapFreeRatio))";

/// Resolve the launch encoding name the way the original's encoding
/// block does: Java 18+ uses the native encoding, older uses
/// file.encoding; "GBK" is routed through GB18030 and reported back as
/// "GBK" (codepage 54936). .NET validates the name against its encoding
/// table; the port recognizes the common names and otherwise falls
/// back, like the original's catch block. Returns
/// (launch_encoding_name, use_compat_encoding).
fn resolve_launch_encoding(input: &JvmInput) -> (String, bool) {
    let use_compat = input.java_major >= 18;
    let name = if use_compat {
        input.native_encoding.as_deref()
    } else {
        input.file_encoding.as_deref()
    };
    let resolved = name.and_then(|n| {
        let lower = n.to_lowercase();
        match lower.as_str() {
            "gbk" | "gb18030" => Some("GBK".to_string()),
            "utf-8" | "utf-16" | "utf-16be" | "utf-32" | "gb2312" | "big5"
            | "shift_jis" | "windows-1252" | "iso-8859-1" | "us-ascii"
            | "windows-936" => Some(lower),
            _ => None,
        }
    });
    (
        resolved.unwrap_or_else(|| input.fallback_encoding_name.clone()),
        use_compat,
    )
}

/// Port of `McLaunchArgumentsJVM` (construction only; the original
/// also logs, extracts jar resources and performs the authlib prefetch
/// request — those inputs arrive pre-made in `JvmInput`).
pub fn jvm_arguments(chain: &[ChainLevel], input: &JvmInput) -> Result<Vec<String>, LaunchError> {
    let mut args: Vec<String> = Vec::new();

    let selected_has_jvm = chain
        .first()
        .map(|level| arguments_array(level.json, "jvm").is_some())
        .unwrap_or(false);
    if selected_has_jvm {
        for level in chain {
            if let Some(entries) = arguments_array(level.json, "jvm") {
                args.extend(
                    eval_argument_entries(entries, &input.os_version)
                        .iter()
                        .map(|s| s.trim().to_string()),
                );
            }
            if level.inherit_name.is_empty() {
                break;
            }
        }
        args = args.iter().map(|a| quote_arg(a)).collect();
    } else {
        args.push(
            "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump"
                .to_string(),
        );
        args.push("-Djava.library.path=${natives_directory}".to_string());
        args.push("-cp".to_string());
        args.push("${classpath}".to_string());
    }

    // Custom arguments: re-split everything together, then dedup.
    let joined = format!(
        "{} {}",
        args.join(" ").replace("McEmu= ", "McEmu="),
        input.custom_arg
    );
    args = deduplicate_java_arguments(split_java_arguments(&joined), true);

    // 统一通行证 agent.
    if let JvmLogin::Nide { server_id } = &input.login {
        args.push(format!(
            "-javaagent:\"${{pure_directory}}\\nide8auth.jar\"=\"{server_id}\""
        ));
    }
    // Authlib-Injector agent.
    if let JvmLogin::Auth { server, prefetched_response } = &input.login {
        let Some(response) = prefetched_response else {
            return Err(LaunchError::AuthPrefetchMissing);
        };
        if input.java_version.major >= 6 {
            args.push("-Djavax.net.ssl.trustStoreType=WINDOWS-ROOT".to_string());
        }
        args.push(format!(
            "-javaagent:\"${{pure_directory}}\\authlib-injector.jar\"=\"{server}\""
        ));
        args.push("-Dauthlibinjector.side=client".to_string());
        args.push(format!(
            "-Dauthlibinjector.yggdrasil.prefetched={}",
            base64_encode(response.as_bytes())
        ));
    }

    // JLW (Java Launch Wrapper) decision.
    let mut use_jlw = !input.disable_jlw_global
        && !input.disable_jlw_instance
        && !input.is_gbk_encoding;
    if use_jlw && input.custom_arg.contains("-javaagent") {
        use_jlw = false;
    }
    if use_jlw {
        if input.java_major >= 9 {
            args.push("--add-exports".to_string());
            args.push("cpw.mods.bootstraplauncher/cpw.mods.bootstraplauncher=ALL-UNNAMED".to_string());
        }
        args.push("-Doolloo.jlw.tmpdir=${pure_directory}".to_string());
    }

    // LUA (Lwjgl Unsafe Agent).
    if input.use_lua {
        if let Some(path) = &input.lua_jar_path {
            args.push(format!("-javaagent:\"{path}\""));
        }
    }

    // Memory.
    args.push(format!("-Xmx{}m", input.ram_mb));

    // GC selection.
    let mut setup_type = input.gc_global;
    if let Some(v) = input.gc_instance_override {
        if v > 0 {
            setup_type = v - 1;
        }
    }
    if setup_type != 3 {
        let mut use_g1gc = (setup_type == 0 && input.java_major < 15)
            || (setup_type == 1 && input.java_major < 21)
            || setup_type == 2
            || setup_type == 4;
        if input.os_major < 10 || input.os_build < 17763 {
            use_g1gc = true;
        }
        let gc_re = regex::Regex::new(GC_ARG_PATTERN).expect("constant pattern");
        args.retain(|a| !gc_re.is_match(a));
        args.push("-XX:+UnlockExperimentalVMOptions".to_string());
        if input.java_major >= 24 {
            args.push("-XX:+UseCompactObjectHeaders".to_string());
        }
        if use_g1gc && setup_type == 4 {
            args.push("-XX:+UseG1GC".to_string());
            args.push("-XX:G1NewSizePercent=20".to_string());
            args.push("-XX:G1ReservePercent=20".to_string());
            args.push("-XX:G1HeapRegionSize=32M".to_string());
            args.push("-XX:MaxGCPauseMillis=50".to_string());
            if !input.mode_debug {
                args.push("-XX:+PerfDisableSharedMem".to_string());
            }
            if input.java_major <= 7 {
                args.push("-XX:MaxPermSize=512m".to_string());
            }
            if input.java_major == 8 {
                args.push("-XX:+ParallelRefProcEnabled".to_string());
            }
            if input.java_major >= 12 {
                args.push("-XX:MinHeapFreeRatio=25".to_string());
                args.push("-XX:MaxHeapFreeRatio=40".to_string());
            }
        } else if use_g1gc {
            args.push("-XX:+UseG1GC".to_string());
            args.push("-XX:G1NewSizePercent=20".to_string());
            args.push("-XX:G1ReservePercent=20".to_string());
            args.push("-XX:G1HeapRegionSize=32M".to_string());
            args.push("-XX:MaxGCPauseMillis=50".to_string());
            if input.java_major <= 7 {
                args.push("-XX:MaxPermSize=512m".to_string());
            }
        } else {
            args.push("-XX:+UseZGC".to_string());
            if input.java_major == 21 || input.java_major == 22 {
                args.push("-XX:+ZGenerational".to_string());
            }
        }
    }

    // List.Remove removes only the first occurrence (#3511 cleanup).
    if let Some(pos) = args.iter().position(|a| a == "-XX:MaxDirectMemorySize=256M") {
        args.remove(pos);
    }

    // Log4j mitigation.
    args.push("-Dlog4j2.formatMsgNoLookups=true".to_string());

    // Encoding arguments.
    let (launch_encoding_name, use_compat) = resolve_launch_encoding(input);
    let stdout_name = if input.java_major < 19 { "sun.stdout.encoding" } else { "stdout.encoding" };
    let stderr_name = if input.java_major < 19 { "sun.stderr.encoding" } else { "stderr.encoding" };
    if !args.iter().any(|a| a.starts_with(&format!("-D{stdout_name}="))) {
        args.push(format!("-D{stdout_name}={launch_encoding_name}"));
    }
    if !args.iter().any(|a| a.starts_with(&format!("-D{stderr_name}="))) {
        args.push(format!("-D{stderr_name}={launch_encoding_name}"));
    }
    if use_compat && !args.iter().any(|a| a.starts_with("-Dfile.encoding=")) {
        args.push("-Dfile.encoding=COMPAT".to_string());
    }

    // JLW wrapper jar goes last.
    if use_jlw {
        if let Some(path) = &input.jlw_jar_path {
            args.push("-jar".to_string());
            args.push(path.clone());
        }
    }

    Ok(deduplicate_java_arguments(args, true))
}

// ---------------------------------------------------------------------------
// McLaunchArgumentsGame.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GameInput {
    pub os_version: String,
    /// Custom game arguments from settings, already run through
    /// `argument_replace` by the caller ("" when unset).
    pub custom_arg: String,
    pub extra_game_args: Vec<String>,
    /// `LaunchArgumentWindowType == 0` (fullscreen).
    pub fullscreen: bool,
    /// Server to auto-join (launch options override the version
    /// setting; the caller resolves that precedence).
    pub server: Option<String>,
    pub release_time: crate::java::SimpleDate,
    pub has_optifine: bool,
    /// Forge or LiteLoader installed (for the tweaker reordering).
    pub forge_or_liteloader: bool,
    pub optifine_as_mod: bool,
}

/// Port of `McLaunchArgumentsGame`. The original also rewrites the
/// version JSON file on disk when it finds the plain OptiFine tweaker
/// next to Forge/LiteLoader — a side effect the port deliberately
/// leaves to the caller; the in-string replacement is done.
pub fn game_arguments(chain: &[ChainLevel], input: &GameInput) -> Vec<String> {
    let mut arg = String::new();
    let selected = chain.first().map(|level| level.json);

    // Legacy template from the (resolved) selected JSON.
    if let Some(selected) = selected {
        if let Some(basic) = token_string(selected.get("minecraftArguments")) {
            if !basic.is_empty() {
                let basic = format!(
                    "{basic} --height ${{resolution_height}} --width ${{resolution_width}}"
                );
                arg.push(' ');
                arg.push_str(&basic);
            }
        }
    }
    // New-style game arguments across the chain.
    if selected.map(|s| arguments_array(s, "game").is_some()).unwrap_or(false) {
        for level in chain {
            if let Some(entries) = arguments_array(level.json, "game") {
                for entry in eval_argument_entries(entries, &input.os_version) {
                    arg.push(' ');
                    arg.push_str(&quote_arg(&entry));
                }
            }
            if level.inherit_name.is_empty() {
                break;
            }
        }
    }

    // Custom arguments.
    arg.push(' ');
    arg.push_str(&input.custom_arg);

    // Keep the OptiFine Forge tweaker at the very end.
    if input.forge_or_liteloader && input.has_optifine && !input.optifine_as_mod {
        if arg.contains("--tweakClass optifine.OptiFineForgeTweaker") {
            arg = arg.replace(" --tweakClass optifine.OptiFineForgeTweaker", "");
            arg.push_str(" --tweakClass optifine.OptiFineForgeTweaker");
        }
        if arg.contains("--tweakClass optifine.OptiFineTweaker") {
            arg = arg.replace(" --tweakClass optifine.OptiFineTweaker", "");
            arg.push_str(" --tweakClass optifine.OptiFineForgeTweaker");
        }
    }

    let mut args = deduplicate_java_arguments(split_java_arguments(&arg), false);

    args.extend(input.extra_game_args.iter().cloned());
    if input.fullscreen {
        args.push("--fullscreen".to_string());
    }
    if let Some(server) = &input.server {
        if !server.is_empty() {
            if input.release_time > (2023, 4, 4) {
                args.push("--quickPlayMultiplayer".to_string());
                args.push(server.clone());
            } else if server.contains(':') {
                let parts: Vec<&str> = server.split(':').collect();
                args.push("--server".to_string());
                args.push(parts[0].to_string());
                args.push("--port".to_string());
                args.push(parts.get(1).copied().unwrap_or("25565").to_string());
            } else {
                args.push("--server".to_string());
                args.push(server.clone());
                args.push("--port".to_string());
                args.push("25565".to_string());
            }
        }
    }

    deduplicate_java_arguments(args, false)
}

// ---------------------------------------------------------------------------
// McLaunchArgumentsReplace + McLaunchArgumentMain.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ReplacementContext {
    /// `${natives_directory}` — slash suffix already removed.
    pub natives_directory: String,
    /// `${library_directory}` / `${libraries_directory}` — the caller
    /// applies ToShortPath, as the original does.
    pub library_directory: String,
    /// `${pure_directory}` — slash suffix already removed.
    pub pure_directory: String,
    pub launcher_version: String,
    pub version_name: String,
    /// `${game_directory}` — short path of the instance folder without
    /// its trailing separator.
    pub game_directory: String,
    pub assets_root: String,
    /// `${game_assets}` — `assets\virtual\legacy` under the assets root.
    pub game_assets: String,
    pub auth_player_name: String,
    pub auth_uuid: String,
    pub auth_access_token: String,
    pub primary_jar: String,
    pub assets_index_name: String,
    /// The custom version-type text from settings ("" when unset).
    pub argument_info: String,
    /// `argument_replace(argument_info)`, precomputed by the caller.
    pub argument_info_replaced: String,
    pub resolution_width: i64,
    pub resolution_height: i64,
    /// `${classpath}` from `instance::build_classpath`.
    pub classpath: String,
}

/// Port of `McLaunchArgumentsReplace`: the replacement table, in the
/// original's yield order (application order matters when a value
/// itself contains a placeholder). When `argument_info` is empty the
/// `--versionType <value>` pair is removed from `arguments` (the
/// original mutates the list it was given) and `${version_type}`
/// maps to a bare `""`.
pub fn build_replacements(
    arguments: &mut Vec<String>,
    ctx: &ReplacementContext,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![
        ("${classpath_separator}".into(), ";".into()),
        ("${natives_directory}".into(), ctx.natives_directory.clone()),
        ("${library_directory}".into(), ctx.library_directory.clone()),
        ("${libraries_directory}".into(), ctx.library_directory.clone()),
        ("${pure_directory}".into(), ctx.pure_directory.clone()),
        ("${launcher_name}".into(), "PCL".into()),
        ("${launcher_version}".into(), ctx.launcher_version.clone()),
        ("${version_name}".into(), ctx.version_name.clone()),
        ("${game_directory}".into(), ctx.game_directory.clone()),
        ("${assets_root}".into(), ctx.assets_root.clone()),
        ("${user_properties}".into(), "{}".into()),
        ("${auth_player_name}".into(), ctx.auth_player_name.clone()),
        ("${auth_uuid}".into(), ctx.auth_uuid.clone()),
        ("${auth_access_token}".into(), ctx.auth_access_token.clone()),
        ("${access_token}".into(), ctx.auth_access_token.clone()),
        ("${auth_session}".into(), ctx.auth_access_token.clone()),
        ("${user_type}".into(), "msa".into()),
        ("${primary_jar}".into(), ctx.primary_jar.clone()),
        ("${game_assets}".into(), ctx.game_assets.clone()),
        ("${assets_index_name}".into(), ctx.assets_index_name.clone()),
    ];
    if ctx.argument_info.is_empty() {
        if let Some(index) = arguments.iter().position(|a| a == "--versionType") {
            arguments.remove(index);
            if index < arguments.len() {
                arguments.remove(index);
            }
        }
        out.push(("${version_type}".into(), "\"\"".into()));
    } else {
        out.push(("${version_type}".into(), ctx.argument_info_replaced.clone()));
    }
    out.push(("${resolution_width}".into(), ctx.resolution_width.to_string()));
    out.push(("${resolution_height}".into(), ctx.resolution_height.to_string()));
    out.push(("${classpath}".into(), ctx.classpath.clone()));
    out
}

/// Port of `McLaunchArgumentMain`'s assembly loop: JVM arguments, the
/// main class, then game arguments; every argument gets all
/// replacements applied in order and is quoted when it contains any
/// of `& | < > ^` or a space (and is not already quoted).
pub fn assemble_launch_arguments(
    jvm_args: Vec<String>,
    main_class: &str,
    game_args: Vec<String>,
    replacements: &[(String, String)],
) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    all.extend(jvm_args);
    all.push(main_class.to_string());
    all.extend(game_args);
    all.iter()
        .map(|arg| {
            let mut arg = arg.clone();
            for (key, value) in replacements {
                arg = arg.replace(key.as_str(), value);
            }
            if arg.chars().any(|c| "&|<>^ ".contains(c))
                && !arg.starts_with('"')
                && !arg.ends_with('"')
            {
                arg = format!("\"{}\"", arg.replace('"', "\\\""));
            }
            arg
        })
        .collect()
}

// ---------------------------------------------------------------------------
// ModMain.vb ArgumentReplace — the {token} substitution for custom
// arguments / commands. Values arrive precomputed (hashes, date/time,
// random hint/cave are the caller's); with no EscapeHandler the
// replacement values pass through unchanged, as in PCL's launch usage.
// When hint/cave are not supplied the {hint}/{cave} tokens are left
// untouched (the original always substitutes a random entry).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ArgumentReplaceInstance {
    pub path_version: String,
    pub path_indie: String,
    pub name: String,
    pub vanilla_name: String,
    pub forge: Option<String>,
    pub fabric: Option<String>,
    pub neoforge: Option<String>,
    pub has_liteloader: bool,
}

#[derive(Debug, Clone)]
pub struct ArgumentReplaceLogin {
    pub user_name: String,
    pub uuid: String,
    /// One of 离线 / 正版 / 统一通行证 / Authlib-Injector (McLoginType).
    pub login_label: String,
}

#[derive(Debug, Clone, Default)]
pub struct ArgumentReplaceContext {
    pub pcl_version_name: String,
    pub pcl_version_code: String,
    pub pcl_version_branch: String,
    pub pcl_build_type: String,
    pub pcl_branch: String,
    pub identify: String,
    pub path_base: String,
    pub path_exe: String,
    pub path_temp: String,
    pub exe_md5: Option<String>,
    pub exe_sha1: Option<String>,
    /// Preformatted like the original: date `yyyy/M/d`, time `HH:mm:ss`.
    pub date: Option<String>,
    pub time: Option<String>,
    pub replace_time: bool,
    pub java_folder: Option<String>,
    pub mc_folder: String,
    pub instance: Option<ArgumentReplaceInstance>,
    pub login: Option<ArgumentReplaceLogin>,
    pub hint: Option<String>,
    pub cave: Option<String>,
    /// `{setup:NAME}` lookups.
    pub setup: HashMap<String, String>,
    /// `{varible:NAME}` / `{variable:NAME}` registry-backed values.
    pub variables: HashMap<String, String>,
}

pub fn argument_replace(text: &str, ctx: &ArgumentReplaceContext) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    let val = |o: &Option<String>| o.clone().unwrap_or_default();
    let mut t = text.to_string();
    let rep = |t: &mut String, token: &str, value: &str| {
        *t = t.replace(token, value);
    };
    rep(&mut t, "{pcl_version}", &ctx.pcl_version_name);
    rep(&mut t, "{pcl_version_code}", &ctx.pcl_version_code);
    rep(&mut t, "{pcl_version_branch}", &ctx.pcl_version_branch);
    rep(&mut t, "{pcl_build_type}", &ctx.pcl_build_type);
    rep(&mut t, "{pcl_branch}", &ctx.pcl_branch);
    rep(&mut t, "{identify}", &ctx.identify);
    rep(&mut t, "{path}", &ctx.path_base);
    rep(&mut t, "{path_with_name}", &ctx.path_exe);
    rep(&mut t, "{path_temp}", &ctx.path_temp);
    rep(&mut t, "{pcl_md5}", &val(&ctx.exe_md5));
    rep(&mut t, "{pcl_sha1}", &val(&ctx.exe_sha1));
    if ctx.replace_time {
        rep(&mut t, "{date}", &val(&ctx.date));
        rep(&mut t, "{time}", &val(&ctx.time));
    }
    rep(&mut t, "{java}", &val(&ctx.java_folder));
    rep(&mut t, "{minecraft}", &ctx.mc_folder);
    match &ctx.instance {
        Some(inst) => {
            rep(&mut t, "{version_path}", &inst.path_version);
            rep(&mut t, "{verpath}", &inst.path_version);
            rep(&mut t, "{version_indie}", &inst.path_indie);
            rep(&mut t, "{verindie}", &inst.path_indie);
            rep(&mut t, "{name}", &inst.name);
            let lower = inst.vanilla_name.to_lowercase();
            let version_value = if ["unknown", "old", "pending"].contains(&lower.as_str()) {
                inst.name.clone()
            } else {
                inst.vanilla_name.clone()
            };
            rep(&mut t, "{version}", &version_value);
            rep(&mut t, "{forge}", &val(&inst.forge));
            rep(&mut t, "{fabric}", &val(&inst.fabric));
            rep(&mut t, "{neoforge}", &val(&inst.neoforge));
            rep(&mut t, "{liteloader}", if inst.has_liteloader { "true" } else { "" });
        }
        None => {
            for token in [
                "{version_path}",
                "{verpath}",
                "{version_indie}",
                "{verindie}",
                "{name}",
                "{version}",
                "{forge}",
                "{fabric}",
                "{neoforge}",
                "{liteloader}",
            ] {
                rep(&mut t, token, "");
            }
        }
    }
    match &ctx.login {
        Some(login) => {
            rep(&mut t, "{user}", &login.user_name);
            rep(&mut t, "{uuid}", &login.uuid.to_lowercase());
            rep(&mut t, "{login}", &login.login_label);
        }
        None => {
            rep(&mut t, "{user}", "");
            rep(&mut t, "{uuid}", "");
            rep(&mut t, "{login}", "");
        }
    }
    if let Some(hint) = &ctx.hint {
        t = t.replace("{hint}", hint);
    }
    if let Some(cave) = &ctx.cave {
        t = t.replace("{cave}", cave);
    }
    // {setup:NAME}
    if t.contains("{setup:") {
        let re = regex::Regex::new(r"\{setup:([a-zA-Z0-9]+)\}").expect("constant pattern");
        t = re
            .replace_all(&t, |caps: &regex::Captures| {
                ctx.setup.get(&caps[1]).cloned().unwrap_or_default()
            })
            .to_string();
    }
    // {varible:NAME[:default]} and {variable:...}
    if t.contains("{varible:") || t.contains("{variable:") {
        let re = regex::Regex::new(r"\{varible:([^:\}]+)(?::([^\}]+))?\}")
            .expect("constant pattern");
        let lookup = |ctx: &ArgumentReplaceContext, caps: &regex::Captures| -> String {
            let name = &caps[1];
            let default = caps.get(2).map(|m| m.as_str().to_string());
            ctx.variables
                .get(name)
                .cloned()
                .unwrap_or_else(|| default.unwrap_or_default())
        };
        t = re.replace_all(&t, |caps: &regex::Captures| lookup(ctx, caps)).to_string();
        let re2 = regex::Regex::new(r"\{variable:([^:\}]+)(?::([^\}]+))?\}")
            .expect("constant pattern");
        t = re2.replace_all(&t, |caps: &regex::Captures| lookup(ctx, caps)).to_string();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_respects_quotes_and_escapes() {
        let args = split_java_arguments(r#"-Dfoo=a b --name "two words" -Dq=\"x\" plain"#);
        assert_eq!(
            args,
            vec!["-Dfoo=a", "b", "--name", "\"two words\"", "-Dq=\\\"x\\\"", "plain"]
        );
        assert!(split_java_arguments("  \r\n ").is_empty());
        assert_eq!(split_java_arguments("-a\r\n-b"), vec!["-a", "-b"]);
    }

    #[test]
    fn dedup_singles_pairs_and_negative_values() {
        // Duplicate single dropped; duplicate pair with same value dropped.
        let out = deduplicate_java_arguments(
            vec!["-a".into(), "-a".into(), "--width".into(), "854".into(), "--width".into(), "854".into()],
            false,
        );
        assert_eq!(out, vec!["-a", "--width", "854"]);
        // Game mode: a conflicting pair overwrites in place.
        let out = deduplicate_java_arguments(
            vec!["--width".into(), "854".into(), "--width".into(), "1920".into()],
            false,
        );
        assert_eq!(out, vec!["--width", "1920"]);
        // JVM mode: a conflicting pair is kept twice.
        let out = deduplicate_java_arguments(
            vec!["-Xmx".into(), "1G".into(), "-Xmx".into(), "2G".into()],
            true,
        );
        assert_eq!(out, vec!["-Xmx", "1G", "-Xmx", "2G"]);
        // "-50" is a value, not a flag: -xPos -50 forms a pair.
        let out = deduplicate_java_arguments(
            vec!["-xPos".into(), "23".into(), "-xPos".into(), "-50".into()],
            false,
        );
        assert_eq!(out, vec!["-xPos", "-50"]);
        // tweakClass pairs are exempt from game-mode overwriting.
        let out = deduplicate_java_arguments(
            vec![
                "--tweakClass".into(), "a.B".into(),
                "--tweakClass".into(), "c.D".into(),
            ],
            false,
        );
        assert_eq!(out.len(), 4);
    }

    fn jvm_input() -> JvmInput {
        JvmInput {
            os_version: "Microsoft Windows NT 10.0.22631.0".into(),
            custom_arg: String::new(),
            login: JvmLogin::None,
            java_major: 21,
            java_version: JavaVersion::dotnet3(21, 0, 7),
            disable_jlw_global: false,
            disable_jlw_instance: false,
            is_gbk_encoding: false,
            jlw_jar_path: Some(r"C:\PCL\JavaWrapper.jar".into()),
            use_lua: false,
            lua_jar_path: None,
            ram_mb: 4096,
            gc_global: 0,
            gc_instance_override: None,
            os_major: 10,
            os_build: 22631,
            mode_debug: false,
            file_encoding: Some("utf-8".into()),
            native_encoding: Some("utf-8".into()),
            fallback_encoding_name: "windows-1252".into(),
        }
    }

    #[test]
    fn jvm_legacy_arguments_and_gc() {
        let json = json!({"id": "1.12.2", "minecraftArguments": "--legacy"});
        let chain = vec![ChainLevel { json: &json, inherit_name: "" }];
        let args = jvm_arguments(&chain, &jvm_input()).unwrap();
        assert_eq!(
            args[..4],
            [
                "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump",
                "-Djava.library.path=${natives_directory}",
                "-cp",
                "${classpath}"
            ]
        );
        assert!(args.contains(&"-Xmx4096m".to_string()));
        // Java 21 + setup 0 -> generational ZGC.
        assert!(args.contains(&"-XX:+UseZGC".to_string()));
        assert!(args.contains(&"-XX:+ZGenerational".to_string()));
        assert!(args.contains(&"-Dlog4j2.formatMsgNoLookups=true".to_string()));
        // Java 21 uses stdout.encoding and COMPAT file.encoding.
        assert!(args.contains(&"-Dstdout.encoding=utf-8".to_string()));
        assert!(args.contains(&"-Dfile.encoding=COMPAT".to_string()));
        // JLW: add-exports pair right before its tmpdir arg; -jar is last.
        let pos = args.iter().position(|a| a == "--add-exports").unwrap();
        assert_eq!(args[pos + 1], "cpw.mods.bootstraplauncher/cpw.mods.bootstraplauncher=ALL-UNNAMED");
        assert_eq!(args[args.len() - 2], "-jar");
        assert_eq!(args[args.len() - 1], r"C:\PCL\JavaWrapper.jar");
    }

    #[test]
    fn jvm_new_style_chain_and_rules() {
        let parent = json!({
            "id": "1.20.1",
            "arguments": {"jvm": [
                "-Dparent=1",
                {"rules": [{"action": "allow", "os": {"name": "osx"}}], "value": "-Dosxonly=1"},
                {"rules": [{"action": "allow", "os": {"name": "windows"}}], "value": ["-Dwin=1", "-Dwin2=2"]}
            ]}
        });
        let child = json!({
            "id": "fabric-1.20.1", "inheritsFrom": "1.20.1",
            "arguments": {"jvm": ["-Dchild=1", " -Dspaced=has space "]}
        });
        let chain = vec![
            ChainLevel { json: &child, inherit_name: "1.20.1" },
            ChainLevel { json: &parent, inherit_name: "" },
        ];
        let args = jvm_arguments(&chain, &jvm_input()).unwrap();
        assert!(args.contains(&"-Dchild=1".to_string()));
        assert!(args.contains(&"-Dparent=1".to_string()));
        assert!(!args.contains(&"-Dosxonly=1".to_string()));
        assert!(args.contains(&"-Dwin=1".to_string()));
        // Entries with spaces get trimmed, then quoted.
        assert!(args.contains(&"\"-Dspaced=has space\"".to_string()));
    }

    #[test]
    fn game_arguments_legacy_and_new() {
        let mut input = GameInput {
            os_version: "Microsoft Windows NT 10.0.22631.0".into(),
            custom_arg: String::new(),
            extra_game_args: vec![],
            fullscreen: false,
            server: None,
            release_time: (2023, 6, 1),
            has_optifine: false,
            forge_or_liteloader: false,
            optifine_as_mod: false,
        };
        let legacy = json!({"id": "1.12.2", "minecraftArguments": "--username ${auth_player_name} --version ${version_name}"});
        let chain = vec![ChainLevel { json: &legacy, inherit_name: "" }];
        let args = game_arguments(&chain, &input);
        assert_eq!(
            args,
            vec![
                "--username", "${auth_player_name}", "--version", "${version_name}",
                "--height", "${resolution_height}", "--width", "${resolution_width}"
            ]
        );
        // Modern JSON + QuickPlay server join.
        let modern = json!({"id": "1.20.1", "arguments": {"game": ["--quickPlay", {"rules": [{"action": "allow", "features": {"is_quick_play_singleplayer": true}}], "value": "--qp"}]}});
        let chain = vec![ChainLevel { json: &modern, inherit_name: "" }];
        input.server = Some("play.example.com".into());
        let args = game_arguments(&chain, &input);
        assert!(args.contains(&"--quickPlay".to_string()));
        assert!(!args.contains(&"--qp".to_string()));
        let pos = args.iter().position(|a| a == "--quickPlayMultiplayer").unwrap();
        assert_eq!(args[pos + 1], "play.example.com");
        // Old-style server split.
        input.release_time = (2020, 1, 1);
        input.server = Some("mc.example.com:25566".into());
        let args = game_arguments(&chain, &input);
        let pos = args.iter().position(|a| a == "--server").unwrap();
        assert_eq!(args[pos + 1], "mc.example.com");
        assert_eq!(args[pos + 3], "25566");
    }

    #[test]
    fn replacements_and_final_assembly() {
        let mut args = vec![
            "-cp".to_string(),
            "${classpath}".to_string(),
            "--versionType".to_string(),
            "${version_type}".to_string(),
            "--username".to_string(),
            "${auth_player_name}".to_string(),
        ];
        let ctx = ReplacementContext {
            natives_directory: r"C:\MC\versions\1.20.1\natives".into(),
            library_directory: r"C:\MC\libraries".into(),
            pure_directory: r"C:\PCL".into(),
            launcher_version: "2.99".into(),
            version_name: "1.20.1".into(),
            game_directory: r"C:\MC\versions\1.20.1".into(),
            assets_root: r"C:\MC\assets".into(),
            game_assets: r"C:\MC\assets\virtual\legacy".into(),
            auth_player_name: "Steve Jobs".into(),
            auth_uuid: "uuid-1".into(),
            auth_access_token: "token".into(),
            primary_jar: r"C:\MC\versions\1.20.1\1.20.1.jar".into(),
            assets_index_name: "5".into(),
            argument_info: String::new(),
            argument_info_replaced: String::new(),
            resolution_width: 854,
            resolution_height: 480,
            classpath: r"C:\MC\libraries\a.jar;C:\MC\versions\1.20.1\1.20.1.jar".into(),
        };
        let replacements = build_replacements(&mut args, &ctx);
        // Empty argument info removes the --versionType pair.
        assert!(!args.contains(&"--versionType".to_string()));
        let final_args = assemble_launch_arguments(vec![], "net.minecraft.client.main.Main", args, &replacements);
        assert!(final_args.contains(&r"C:\MC\libraries\a.jar;C:\MC\versions\1.20.1\1.20.1.jar".to_string()));
        // A value with a space is quoted at assembly.
        assert!(final_args.contains(&"\"Steve Jobs\"".to_string()));
    }

    #[test]
    fn argument_replace_tokens() {
        let mut ctx = ArgumentReplaceContext {
            pcl_version_name: "PCL 2.99".into(),
            mc_folder: r"C:\MC\".into(),
            replace_time: true,
            date: Some("2026/11/14".into()),
            time: Some("23:05:00".into()),
            ..Default::default()
        };
        ctx.instance = Some(ArgumentReplaceInstance {
            path_version: r"C:\MC\versions\1.20.1\".into(),
            path_indie: r"C:\MC\versions\1.20.1\".into(),
            name: "1.20.1".into(),
            vanilla_name: "Unknown".into(),
            forge: None,
            fabric: Some("0.16.9".into()),
            neoforge: None,
            has_liteloader: false,
        });
        ctx.login = Some(ArgumentReplaceLogin {
            user_name: "Steve".into(),
            uuid: "ABCDEF".into(),
            login_label: "正版".into(),
        });
        ctx.setup.insert("LaunchRam".into(), "4096".into());
        let out = argument_replace(
            "run {name} ver {version} by {user} ({login}) at {date} ram {setup:LaunchRam} fabric {fabric} plain",
            &ctx,
        );
        assert_eq!(
            out,
            "run 1.20.1 ver 1.20.1 by Steve (正版) at 2026/11/14 ram 4096 fabric 0.16.9 plain"
        );
        // No '{' -> returned untouched.
        assert_eq!(argument_replace("nothing", &ctx), "nothing");
    }
}
