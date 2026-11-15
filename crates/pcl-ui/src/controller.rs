//! UI controller (M3 batch 1): bridges the Slint front-end to
//! `pcl-core`.
//!
//! - Instance list: [`scan_workspace`] finds a `.minecraft` folder
//!   (platform candidates) and runs `instance::scan_instances`.
//! - Launch button: [`launch_flow`] walks the same chain the CLI
//!   walks — `precheck` (the licence-required `McLaunchPrecheck`
//!   equivalent) → offline `login::legacy_login` → inheritance
//!   resolution → library list → natives extraction → Java
//!   requirement + discovery → argument assembly →
//!   `runner::spawn_game` (through the [`Spawner`] trait so tests
//!   can record the spec instead of starting a process).
//!
//! Batch-1 scope cuts, deliberately visible in the log instead of
//! hidden: no download/fix step (missing files are reported, the
//! CLI's `install` covers it), only the offline (Legacy) login is
//! wired, and the non-Chinese demo gate stops the chain with an
//! explanation instead of a dialog.

use std::path::{Path, PathBuf};

use serde_json::Value;

use pcl_core::account::{McLoginData, McLoginLegacy, McLoginType};
use pcl_core::instance::{
    build_classpath, lib_list_from_json_os, resolve_inheritance, InstanceSummary, LibEntry,
};
use pcl_core::java::{
    describe_required_java, java_requirement, parse_check_output, JavaRequirementInput, ModLoaders,
};
use pcl_core::launch::{
    assemble_launch_arguments, build_replacements, game_arguments, jvm_arguments, ChainLevel,
    GameInput, JvmInput, JvmLogin, ReplacementContext,
};
use pcl_core::login;
use pcl_core::mcfix;
use pcl_core::platform;
use pcl_core::precheck::{
    precheck, PrecheckInput, PrecheckOutcome, PrecheckPrompt, BUY_URL, DONATE_URL,
};
use pcl_core::runner::{self, SpawnSpec};
use pcl_core::version::name_to_version;
use pcl_core::vrange::JavaVersion;

use crate::platform as ui_platform;

// ---------------------------------------------------------------------------
// Workspace scan.
// ---------------------------------------------------------------------------

/// The `.minecraft` folder in use plus the instances found in it.
pub struct Workspace {
    pub mc_dir: PathBuf,
    pub instances: Vec<InstanceSummary>,
}

/// Pick the first platform candidate that has a `versions` folder
/// (falling back to the first candidate) and scan its instances.
pub fn scan_workspace() -> Workspace {
    let candidates = ui_platform::mc_dir_candidates();
    let mc_dir = candidates
        .iter()
        .find(|c| c.join("versions").is_dir())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone());
    let instances = pcl_core::instance::scan_instances(&mc_dir.join("versions"));
    Workspace { mc_dir, instances }
}

/// Rescan one specific `.minecraft` folder.
pub fn scan_dir(mc_dir: &Path) -> Vec<InstanceSummary> {
    pcl_core::instance::scan_instances(&mc_dir.join("versions"))
}

// ---------------------------------------------------------------------------
// Spawner abstraction (the runner entry point, testable).
// ---------------------------------------------------------------------------

/// Starts the game process from a fully assembled [`SpawnSpec`].
/// Returns the child PID.
pub trait Spawner {
    fn spawn(&self, spec: &SpawnSpec) -> Result<u32, String>;
}

/// Production spawner: `runner::spawn_game`, with a drainer thread
/// forwarding the child's output until it exits (as the CLI does).
pub struct ProcessSpawner;

impl Spawner for ProcessSpawner {
    fn spawn(&self, spec: &SpawnSpec) -> Result<u32, String> {
        let short: &dyn Fn(&str) -> String = if cfg!(target_os = "windows") {
            &pcl_core::pathutil::to_short_path
        } else {
            &|s: &str| s.to_string()
        };
        let mut child = runner::spawn_game(spec, short)?;
        let pid = child.id();
        std::thread::spawn(move || {
            use std::io::Read;
            fn drain(pipe: &mut Option<impl Read>) {
                if let Some(p) = pipe.as_mut() {
                    let mut buf = [0u8; 8192];
                    while matches!(p.read(&mut buf), Ok(n) if n > 0) {}
                }
            }
            let mut stdout = child.stdout.take();
            let mut stderr = child.stderr.take();
            drain(&mut stdout);
            drain(&mut stderr);
            let _ = child.wait();
        });
        Ok(pid)
    }
}

/// Test spawner: records every spec it is handed, starts nothing.
#[cfg(test)]
pub struct RecordingSpawner {
    pub specs: std::cell::RefCell<Vec<SpawnSpec>>,
}

#[cfg(test)]
impl Spawner for RecordingSpawner {
    fn spawn(&self, spec: &SpawnSpec) -> Result<u32, String> {
        self.specs.borrow_mut().push(spec.clone());
        Ok(4242)
    }
}

// ---------------------------------------------------------------------------
// Java discovery (native paths; mirrors the CLI's approach).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FoundJava {
    pub exe: PathBuf,
    pub version: JavaVersion,
    pub file_encoding: Option<String>,
    pub native_encoding: Option<String>,
}

fn probe_java_exe(exe: &Path) -> Option<FoundJava> {
    let output = std::process::Command::new(exe).arg("-version").output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    let probe = parse_check_output(&text).ok()?;
    Some(FoundJava {
        exe: exe.to_path_buf(),
        version: probe.version,
        file_encoding: probe.file_encoding,
        native_encoding: probe.native_encoding,
    })
}

fn collect_java_bins(root: &Path, depth: usize, bins: &mut Vec<PathBuf>) {
    let exe_name = ui_platform::java_exe_name();
    if root.join(exe_name).is_file() {
        bins.push(root.to_path_buf());
    }
    if depth == 0 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_java_bins(&path, depth - 1, bins);
            }
        }
    }
}

/// Discover Java runtimes: search roots (depth-limited, like the
/// CLI) plus any `java` sitting on `PATH`.
pub fn discover_javas() -> Vec<FoundJava> {
    let mut bins: Vec<PathBuf> = Vec::new();
    for root in ui_platform::java_search_roots() {
        collect_java_bins(&root, 3, &mut bins);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            bins.push(dir);
        }
    }
    bins.sort();
    bins.dedup();
    let mut found: Vec<FoundJava> = Vec::new();
    for bin in bins {
        let exe = bin.join(ui_platform::java_exe_name());
        if exe.is_file() {
            if let Some(java) = probe_java_exe(&exe) {
                if !found.iter().any(|j| j.exe == java.exe) {
                    found.push(java);
                }
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Launch chain.
// ---------------------------------------------------------------------------

fn native_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn load_raw_json(mc: &Path, name: &str) -> Result<Value, String> {
    let path = mc.join("versions").join(name).join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("无法读取版本 JSON（{}）：{e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("版本 JSON 解析失败（{name}）：{e}"))
}

struct ChainItem {
    name: String,
    resolved: Value,
    inherit_name: String,
}

/// Same walk as the CLI's `build_chain`: each level's resolved
/// JSON plus its adjusted inherit name, child first.
fn build_chain(mc: &Path, name: &str, log: &mut Vec<String>) -> Result<Vec<ChainItem>, String> {
    let mut items = Vec::new();
    let mut current = name.to_string();
    loop {
        let raw = load_raw_json(mc, &current)?;
        let outcome = resolve_inheritance(&current, raw, &|parent| load_raw_json(mc, parent).ok());
        for warning in &outcome.warnings {
            log.push(format!("警告：{warning}"));
        }
        let next = outcome.inherit_name.clone();
        items.push(ChainItem {
            name: current.clone(),
            resolved: outcome.json,
            inherit_name: next.clone(),
        });
        if next.is_empty() || next == current {
            break;
        }
        current = next;
        if items.len() > 32 {
            return Err("版本继承链过深，可能存在循环继承".to_string());
        }
    }
    Ok(items)
}

fn parse_release_time(resolved: &Value) -> (i32, u32, u32) {
    let raw = resolved
        .get("releaseTime")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut parts = raw.split(['-', 'T']);
    let year = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let month = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    let day = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    (year, month, day)
}

fn detect_chinese_locale() -> bool {
    if let Some(name) = platform::user_locale_name() {
        return name.to_lowercase().starts_with("zh");
    }
    for key in ["LC_ALL", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            return value.to_lowercase().starts_with("zh");
        }
    }
    false
}

/// Run the launch chain for `instance_name` in `mc_dir` with the
/// offline player `player_name`. Every step appends to the returned
/// log; the final step hands a [`SpawnSpec`] to `spawner`.
pub fn launch_flow(
    mc_dir: &Path,
    instance_name: &str,
    player_name: &str,
    launch_count: i64,
    spawner: &dyn Spawner,
) -> Result<Vec<String>, String> {
    let mut log: Vec<String> = Vec::new();
    let mc = mc_dir;
    // PCL's per-instance folders: the batch-1 UI always launches
    // with version isolation (the instance folder is the game dir),
    // matching how the user's instances are laid out.
    let indie = mc.join("versions").join(instance_name);
    let version_path = indie.clone();

    if player_name.trim().is_empty() {
        return Err("离线登录需要填写玩家名".to_string());
    }
    let login_input = McLoginLegacy {
        user_name: player_name.to_string(),
        ..McLoginLegacy::default()
    };
    let login_data = McLoginData::Legacy(login_input.clone());

    // ---- Precheck (McLaunchPrecheck equivalent). ----
    let outcome = precheck(&PrecheckInput {
        indie_path: native_str(&indie),
        version_path: native_str(&version_path),
        instance_error: None,
        login: login_data,
        login_type: McLoginType::Legacy,
        hint_buy: false,
        launch_count,
        is_release_build: true,
        save_batch: false,
        is_chinese_locale: detect_chinese_locale(),
    })?;
    match outcome {
        PrecheckOutcome::Proceed { prompts } => {
            log.push("启动前检查通过".to_string());
            for prompt in prompts {
                match prompt {
                    PrecheckPrompt::Donate => log.push(format!(
                        "提示：PCL 已经为你启动了 {launch_count} 次游戏啦！如果 PCL 还算好用的话，能不能考虑赞助一下 PCL…… {DONATE_URL}"
                    )),
                    PrecheckPrompt::Buy => log.push(format!(
                        "提示：你已经启动了 {launch_count} 次 Minecraft 啦！如果觉得 Minecraft 还不错，可以购买正版支持一下：{BUY_URL}"
                    )),
                }
            }
        }
        PrecheckOutcome::DemoGate => {
            return Err(
                "你必须先登录正版账号，才能进行离线登录！（正版购买确认框将在后续批次接入，本次启动已停在此处）"
                    .to_string(),
            );
        }
    }

    // ---- Offline login. ----
    let login_result = login::legacy_login(&login_input, false, false, &|_| {
        Err(login::LoginError::Message(
            "皮肤类型 3 需要正版 UUID 查询".to_string(),
        ))
    })
    .map_err(|e| e.to_string())?;
    log.push(format!(
        "登录完成：{}（{}，离线）",
        login_result.name, login_result.uuid
    ));

    // ---- Chain, libraries, natives. ----
    let items = build_chain(mc, instance_name, &mut log)?;
    let resolved = &items[0].resolved;
    let base = items.last().unwrap();
    let mc_pcl = mcfix::mc_folder_pcl(mc);
    let entries = lib_list_from_json_os(
        resolved,
        &mc_pcl,
        false,
        platform::host_is_64bit(),
        &platform::host_os_version(),
        platform::host_os_key(),
    )
    .map_err(|e| format!("分析支持库失败：{e}"))?;

    // Batch 1 has no download/fix step: verify the files the chain
    // needs are on disk and say so plainly when they are not.
    let client_jar = mcfix::primary_jar_path(mc, &base.name);
    if !client_jar.is_file() {
        return Err(format!(
            "游戏主文件缺失（{}）。本批 UI 尚未接入下载补齐，请先用 pcl-cli install {} 补齐文件",
            client_jar.display(),
            base.name
        ));
    }
    for entry in &entries {
        let path = mcfix::to_native_path(&entry.local_path);
        if !path.is_file() {
            return Err(format!(
                "支持库文件缺失（{}）。本批 UI 尚未接入下载补齐，请先用 pcl-cli install {} 补齐文件",
                path.display(),
                base.name
            ));
        }
    }

    let natives_path = runner::natives_dir(&version_path, instance_name);
    let natives_jars: Vec<PathBuf> = entries
        .iter()
        .filter(|e| e.is_natives)
        .map(|e| mcfix::to_native_path(&e.local_path))
        .collect();
    if !natives_jars.is_empty() {
        let extracted = runner::extract_natives(&natives_jars, &natives_path)?;
        log.push(format!("Natives 解压完成（新解压 {extracted} 个文件）"));
    }

    // ---- Java requirement + discovery. ----
    let has_optifine = entries.iter().any(|e| {
        e.original_name
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains("optifine")
    });
    let requirement = java_requirement(&JavaRequirementInput {
        setting_mode: 0,
        setting_range: None,
        valid: true,
        vanilla: JavaVersion::from_comparable(name_to_version(&base.name)),
        release_time: parse_release_time(resolved),
        json_java_major: resolved
            .get("javaVersion")
            .and_then(|j| j.get("majorVersion"))
            .and_then(Value::as_i64),
        json_java_component: resolved
            .get("javaVersion")
            .and_then(|j| j.get("component"))
            .and_then(Value::as_str)
            .map(str::to_string),
        has_optifine,
        mod_loaders: ModLoaders(0),
        forge_version: None,
        neoforge_version: None,
        fabric_version: None,
        login_is_nide: false,
    });
    let javas = discover_javas();
    let Some(java) = javas.iter().find(|j| requirement.range.contains(j.version)) else {
        let found = if javas.is_empty() {
            "未发现任何 Java".to_string()
        } else {
            javas
                .iter()
                .map(|j| format!("{}（{}）", j.exe.display(), j.version))
                .collect::<Vec<_>>()
                .join("、")
        };
        return Err(format!(
            "没有符合要求的 Java：需要 {}，已发现：{found}",
            describe_required_java(&requirement.range)
        ));
    };
    log.push(format!("使用 Java：{}（{}）", java.exe.display(), java.version));

    // ---- Argument assembly. ----
    let levels: Vec<ChainLevel<'_>> = items
        .iter()
        .map(|item| ChainLevel {
            json: &item.resolved,
            inherit_name: &item.inherit_name,
        })
        .collect();
    let os_version = platform::host_os_version();
    let jvm_input = JvmInput {
        os_version: os_version.clone(),
        custom_arg: String::new(),
        login: JvmLogin::None,
        java_major: java.version.major,
        java_version: java.version,
        disable_jlw_global: false,
        disable_jlw_instance: false,
        is_gbk_encoding: false,
        jlw_jar_path: None,
        use_lua: false,
        lua_jar_path: None,
        ram_mb: 2048,
        gc_global: 4,
        gc_instance_override: None,
        os_major: 10,
        os_build: 0,
        mode_debug: false,
        file_encoding: java.file_encoding.clone(),
        native_encoding: java.native_encoding.clone(),
        fallback_encoding_name: "utf-8".to_string(),
    };
    let jvm_args =
        jvm_arguments(&levels, &jvm_input).map_err(|e| format!("生成 JVM 参数失败：{e}"))?;
    let mut game_args_vec = game_arguments(
        &levels,
        &GameInput {
            os_version,
            custom_arg: String::new(),
            extra_game_args: Vec::new(),
            fullscreen: false,
            server: None,
            release_time: parse_release_time(resolved),
            has_optifine,
            forge_or_liteloader: false,
            optifine_as_mod: false,
        },
    );
    let mut cp_entries = entries.clone();
    cp_entries.push(LibEntry::new(
        Some("client".to_string()),
        format!("{}versions\\{}\\\\{}.jar", mc_pcl, base.name, base.name),
        0,
        false,
        None,
        None,
    ));
    let mut classpath = build_classpath(&cp_entries, &|p| {
        mcfix::to_native_path(p).to_string_lossy().into_owned()
    });
    if !cfg!(target_os = "windows") {
        classpath = classpath.replace(';', ":");
    }
    let index_info = mcfix::asset_index_info(resolved);
    let replacement_ctx = ReplacementContext {
        natives_directory: native_str(&natives_path),
        library_directory: native_str(&mc.join("libraries")),
        pure_directory: native_str(mc),
        launcher_version: "2.99.0.1".to_string(),
        version_name: instance_name.to_string(),
        game_directory: native_str(&indie),
        assets_root: native_str(&mc.join("assets")),
        game_assets: native_str(&mc.join("assets").join("virtual").join("legacy")),
        auth_player_name: login_result.name.clone(),
        auth_uuid: login_result.uuid.clone(),
        auth_access_token: login_result.access_token.clone(),
        primary_jar: native_str(&mcfix::primary_jar_path(mc, &base.name)),
        assets_index_name: index_info.id.clone(),
        argument_info: String::new(),
        argument_info_replaced: String::new(),
        resolution_width: 854,
        resolution_height: 480,
        classpath,
    };
    let mut replacements = build_replacements(&mut game_args_vec, &replacement_ctx);
    if !cfg!(target_os = "windows") {
        for (key, value) in replacements.iter_mut() {
            if key == "${classpath_separator}" {
                *value = ":".to_string();
            }
        }
    }
    let main_class = resolved
        .get("mainClass")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let display_args = assemble_launch_arguments(jvm_args, &main_class, game_args_vec, &replacements);
    let spawn_args: Vec<String> = display_args
        .iter()
        .map(|a| {
            if a.len() >= 2 && a.starts_with('"') && a.ends_with('"') {
                a[1..a.len() - 1].replace("\\\"", "\"")
            } else {
                a.clone()
            }
        })
        .collect();

    // ---- Runner entry. ----
    let spec = SpawnSpec {
        java_exe: java.exe.clone(),
        args: spawn_args,
        working_dir: indie.clone(),
        mc_folder: mc.to_path_buf(),
    };
    let pid = spawner.spawn(&spec)?;
    log.push(format!("游戏进程已启动（PID {pid}）"));
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(path: &Path, content: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
    }

    /// Build a minimal `.minecraft` fixture plus a fake JDK whose
    /// `java` is a shell script echoing a plausible `-version`.
    fn make_fixture() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "rerust-pcl-ui-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let mc = base.join(".minecraft");
        let json = r#"{
            "id": "TestVer",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": [],
            "assetIndex": {"id": "legacy"},
            "releaseTime": "2024-01-01T00:00:00+00:00",
            "javaVersion": {"component": "java-runtime-gamma", "majorVersion": 17},
            "arguments": {"game": [], "jvm": []},
            "minecraftArguments": "--username ${auth_player_name} --gameDir ${game_directory}"
        }"#;
        write_file(
            &mc.join("versions").join("TestVer").join("TestVer.json"),
            json.as_bytes(),
        );
        write_file(
            &mc.join("versions").join("TestVer").join("TestVer.jar"),
            b"fake client jar",
        );
        // Fake JDK: only probed on unix (shell script). On Windows
        // the probe step is skipped by the cfg in the test below.
        let jdk = base.join("fakejdk");
        let script = "#!/bin/sh\necho 'openjdk version \"21.0.1\" 2023-10-17' 1>&2\necho 'OpenJDK Runtime Environment (build 21.0.1+12)' 1>&2\necho 'OpenJDK 64-Bit Server VM (build 21.0.1+12, mixed mode)' 1>&2\n";
        write_file(&jdk.join("bin").join("java"), script.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exe = jdk.join("bin").join("java");
            let mut perm = std::fs::metadata(&exe).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&exe, perm).unwrap();
        }
        (mc, jdk)
    }

    #[test]
    fn scan_finds_fixture_instance() {
        let (mc, _jdk) = make_fixture();
        let instances = scan_dir(&mc);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].folder_name, "TestVer");
        assert_eq!(instances[0].main_class, "net.minecraft.client.main.Main");
    }

    #[cfg(unix)]
    #[test]
    fn launch_chain_reaches_spawner() {
        let (mc, jdk) = make_fixture();
        std::env::set_var("JAVA_HOME", &jdk);
        // Chinese locale: the non-Chinese Legacy demo gate would
        // (correctly) stop the chain before the spawner.
        std::env::set_var("LC_ALL", "zh_CN.UTF-8");
        let spawner = RecordingSpawner {
            specs: std::cell::RefCell::new(Vec::new()),
        };
        let log = launch_flow(&mc, "TestVer", "Steve", 1, &spawner)
            .expect("launch chain should reach the spawner");
        assert!(
            log.iter().any(|l| l.contains("登录完成")),
            "log should record the offline login: {log:?}"
        );
        assert!(
            log.iter().any(|l| l.contains("游戏进程已启动（PID 4242）")),
            "log should record the spawn: {log:?}"
        );
        let specs = spawner.specs.borrow();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].working_dir, mc.join("versions").join("TestVer"));
        assert!(
            specs[0].args.iter().any(|a| a == "net.minecraft.client.main.Main"),
            "args should carry the main class: {:?}",
            specs[0].args
        );
        std::env::remove_var("JAVA_HOME");
        std::env::remove_var("LC_ALL");
    }

    #[test]
    fn launch_rejects_empty_player_name() {
        let (mc, _jdk) = make_fixture();
        let spawner = RecordingSpawner {
            specs: std::cell::RefCell::new(Vec::new()),
        };
        let err = launch_flow(&mc, "TestVer", "  ", 1, &spawner).unwrap_err();
        assert!(err.contains("玩家名"), "unexpected error: {err}");
    }
}
