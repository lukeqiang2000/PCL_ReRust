//! M2 subcommands: `versions`, `install`, `launch`.
//!
//! These wire pcl-core's M2 pieces together the way
//! `McLaunchStart` does upstream: precheck → login → DlClientFix →
//! natives → argument assembly → spawn. Java discovery uses the
//! core's probe (`ProcessJavaRunner` + `parse_check_output`) over
//! host-appropriate candidate folders; the core's own folder search
//! remains Windows-shaped (PCL candidate list + `java.exe`), so the
//! host walk lives here.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;

use pcl_core::account::{
    AccountStore, McLoginData, McLoginLegacy, McLoginMs, McLoginResult, McLoginServer,
    McLoginType, MsAccountList,
};
use pcl_core::download::{
    DownloadEngine, DownloadOptions, FileChecker, NetFile, SourcePreference,
};
use pcl_core::http::{HttpClient, ReqwestClient};
use pcl_core::instance::{
    build_classpath, lib_list_from_json_os, resolve_inheritance, LibEntry,
};
use pcl_core::java::{
    candidate_folders, describe_required_java, java_requirement, parse_check_output,
    JavaCandidateEnv, JavaRequirementInput, JavaRunner, ModLoaders, ProcessJavaRunner,
};
use pcl_core::launch::{
    assemble_launch_arguments, build_replacements, game_arguments, jvm_arguments, ChainLevel,
    GameInput, JvmInput, JvmLogin, ReplacementContext,
};
use pcl_core::login;
use pcl_core::manifest;
use pcl_core::mcfix;
use pcl_core::platform;
use pcl_core::precheck::{
    precheck, DemoGateChoice, PrecheckInput, PrecheckOutcome, PrecheckPrompt, BUY_URL,
    DONATE_URL,
};
use pcl_core::runner;
use pcl_core::version::name_to_version;
use pcl_core::vrange::JavaVersion;

// ---------------------------------------------------------------------------
// Small shared helpers.
// ---------------------------------------------------------------------------

pub struct Opts {
    pub flags: Vec<String>,
    pub values: std::collections::HashMap<String, String>,
}

impl Opts {
    pub fn parse(args: &[String]) -> Opts {
        let mut flags = Vec::new();
        let mut values = std::collections::HashMap::new();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if let Some(name) = arg.strip_prefix("--") {
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    values.insert(name.to_string(), args[i + 1].clone());
                    i += 2;
                } else {
                    flags.push(name.to_string());
                    i += 1;
                }
            } else {
                flags.push(arg.clone());
                i += 1;
            }
        }
        Opts { flags, values }
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn positional(&self, index: usize) -> Option<&str> {
        self.flags.get(index).map(String::as_str)
    }
}

fn default_mc_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MINECRAFT_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join(".minecraft")
}

fn source_preference(opts: &Opts) -> SourcePreference {
    match opts.get("source") {
        Some("0") => SourcePreference::MirrorFirst,
        Some("2") => SourcePreference::OfficialFirst,
        _ => SourcePreference::Auto {
            prefer_official_measured: true,
        },
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn native(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn load_raw_json(mc: &Path, name: &str) -> Result<Value, String> {
    let path = mc
        .join("versions")
        .join(name)
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("无法读取版本 JSON（{}）：{e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("版本 JSON 解析失败（{name}）：{e}"))
}

// ---------------------------------------------------------------------------
// Instance chain.
// ---------------------------------------------------------------------------

struct ChainItem {
    name: String,
    resolved: Value,
    inherit_name: String,
}

fn build_chain(mc: &Path, name: &str) -> Result<Vec<ChainItem>, String> {
    let mut items = Vec::new();
    let mut current = name.to_string();
    loop {
        let raw = load_raw_json(mc, &current)?;
        let outcome = resolve_inheritance(&current, raw, &|parent| load_raw_json(mc, parent).ok());
        for warning in &outcome.warnings {
            eprintln!("警告：{warning}");
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

fn chain_levels(items: &[ChainItem]) -> Vec<ChainLevel<'_>> {
    items
        .iter()
        .map(|item| ChainLevel {
            json: &item.resolved,
            inherit_name: &item.inherit_name,
        })
        .collect()
}

fn indie_dir(mc: &Path, name: &str, isolate: bool) -> PathBuf {
    if isolate {
        mc.join("versions").join(name)
    } else {
        mc.to_path_buf()
    }
}

// ---------------------------------------------------------------------------
// Download plumbing shared by install + launch.
// ---------------------------------------------------------------------------

fn run_downloads(
    engine: &DownloadEngine,
    files: &[NetFile],
    label: &str,
) -> Result<(), String> {
    if files.is_empty() {
        println!("{label}：无需下载");
        return Ok(());
    }
    println!("{label}：{} 个文件", files.len());
    let results = pcl_core::download::download_all(engine, files, &|done, total, file, result| {
        match result {
            Ok(pcl_core::download::DownloadOutcome::Downloaded { bytes, source }) => {
                println!("  [{done}/{total}] {}（{bytes} B，{source}）", file.local_name())
            }
            Ok(pcl_core::download::DownloadOutcome::AlreadyOk) => {}
            Err(e) => eprintln!("  [{done}/{total}] 失败：{e}"),
        }
    });
    let failures: Vec<String> = results
        .iter()
        .filter_map(|r| r.as_ref().err().map(|e| e.to_string()))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} 个文件下载失败：\n{}",
            failures.len(),
            failures.join("\n")
        ))
    }
}

/// The DlClientFix equivalent for one installed instance: client
/// jar + libraries + asset index first, then the asset objects.
fn ensure_instance_files(
    mc: &Path,
    name: &str,
    pref: SourcePreference,
    isolate: bool,
    http: &dyn HttpClient,
) -> Result<(), String> {
    let items = build_chain(mc, name)?;
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

    let engine = DownloadEngine::new(http, DownloadOptions::default());
    let mut files: Vec<NetFile> = Vec::new();
    if let Some(jar) = mcfix::client_jar_net_file(&base.resolved, &base.name, mc, pref) {
        files.push(jar);
    } else {
        eprintln!("警告：未找到主 jar 的下载信息，跳过主 jar 下载");
    }
    files.extend(mcfix::library_net_files(&entries, &mc_pcl, pref));
    let index_info = mcfix::asset_index_info(resolved);
    if let Some(index_file) = mcfix::asset_index_net_file(&index_info, mc, pref) {
        files.push(index_file);
    }
    run_downloads(&engine, &files, "支持库与资源索引")?;

    // Asset objects need the index on disk first.
    let index_path = mc
        .join("assets")
        .join("indexes")
        .join(format!("{}.json", index_info.id));
    match std::fs::read_to_string(&index_path) {
        Ok(index_text) => {
            let assets = mcfix::asset_net_files(
                &index_text,
                mc,
                &indie_dir(mc, name, isolate),
                true,
                pref,
            )?;
            run_downloads(&engine, &assets, "资源文件")?;
        }
        Err(_) => {
            eprintln!(
                "警告：资源索引不存在（{}），跳过资源文件下载，游戏内可能缺失部分资源",
                index_path.display()
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// pcl-cli versions
// ---------------------------------------------------------------------------

pub fn cmd_versions(args: &[String]) -> ExitCode {
    let opts = Opts::parse(args);
    let http = ReqwestClient::new();
    let cache_dir = std::env::temp_dir().join("rerust-pcl");
    match manifest::fetch_manifest(&http, source_preference(&opts), &cache_dir) {
        Ok(outcome) => {
            let m = &outcome.manifest;
            println!("来源：{}{}", outcome.source_name, if outcome.from_cache { "（网络不可用时的回退）" } else { "" });
            println!("最新正式版：{}", m.latest.release);
            println!("最新快照版：{}", m.latest.snapshot);
            println!("版本总数：{}", m.versions.len());
            if let Some(filter) = opts.positional(0) {
                let filter = manifest::normalize_version_id(filter);
                let matches: Vec<&str> = m
                    .versions
                    .iter()
                    .map(|v| v.id.as_str())
                    .filter(|id| id.contains(&filter))
                    .take(20)
                    .collect();
                println!("匹配 “{filter}”：{}", matches.join(", "));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// pcl-cli install <id>
// ---------------------------------------------------------------------------

pub fn cmd_install(args: &[String]) -> ExitCode {
    let opts = Opts::parse(args);
    let Some(id) = opts.positional(0).map(str::to_string) else {
        eprintln!("用法：pcl-cli install <版本 id> [--mc <目录>] [--source 0|1|2]");
        return ExitCode::from(2);
    };
    let mc = opts
        .get("mc")
        .map(PathBuf::from)
        .unwrap_or_else(default_mc_dir);
    let pref = source_preference(&opts);
    let http = ReqwestClient::new();
    let cache_dir = std::env::temp_dir().join("rerust-pcl");
    let outcome = match manifest::fetch_manifest(&http, pref, &cache_dir) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    // Download the version JSON (and any missing parents), then fix.
    let mut to_fetch = vec![manifest::normalize_version_id(&id)];
    let engine = DownloadEngine::new(&http, DownloadOptions::default());
    let mut guard = 0;
    while let Some(want) = to_fetch.pop() {
        guard += 1;
        if guard > 16 {
            eprintln!("继承链过深，停止安装");
            return ExitCode::FAILURE;
        }
        let json_path = mc
            .join("versions")
            .join(&want)
            .join(format!("{want}.json"));
        if !json_path.is_file() {
            let Some(entry) = outcome.manifest.find(&want) else {
                eprintln!("版本列表中不存在 {want}");
                return ExitCode::FAILURE;
            };
            let checker = match &entry.sha1 {
                Some(sha1) => FileChecker::with_hash(sha1.clone()),
                None => FileChecker::default(),
            };
            let file = NetFile::new(
                pcl_core::download::dl_source_launcher_or_meta_get(pref, &entry.url),
                json_path.clone(),
                checker,
            );
            if let Err(e) = run_downloads(&engine, &[file], &format!("版本 JSON {want}")) {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
        if let Ok(raw) = load_raw_json(&mc, &want) {
            if let Some(parent) = raw.get("inheritsFrom").and_then(Value::as_str) {
                let parent_path = mc
                    .join("versions")
                    .join(parent)
                    .join(format!("{parent}.json"));
                if !parent_path.is_file() {
                    to_fetch.push(parent.to_string());
                }
            }
        }
    }
    match ensure_instance_files(&mc, &id, pref, opts.has("isolate"), &http) {
        Ok(()) => {
            println!("安装完成：{id}（{}）", mc.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Java discovery (host walk over the core's probe).
// ---------------------------------------------------------------------------

struct FoundJava {
    exe: PathBuf,
    version: JavaVersion,
    file_encoding: Option<String>,
    native_encoding: Option<String>,
}

fn java_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    }
}

fn probe_java_bin(bin_dir: &Path) -> Option<FoundJava> {
    let exe = bin_dir.join(java_exe_name());
    if !exe.is_file() {
        return None;
    }
    let runner = ProcessJavaRunner;
    let output = runner
        .run(
            &exe.to_string_lossy(),
            &["-XshowSettings:properties", "-version"],
            std::time::Duration::from_secs(15),
        )
        .ok()?;
    let probe = parse_check_output(&output).ok()?;
    Some(FoundJava {
        exe,
        version: probe.version,
        file_encoding: probe.file_encoding,
        native_encoding: probe.native_encoding,
    })
}

/// Collect the bin dirs of one Java root: the root itself when
/// it directly holds the executable, and its `bin` subfolder.
fn push_java_root(root: &Path, bin_dirs: &mut Vec<PathBuf>) {
    if root.join(java_exe_name()).is_file() {
        bin_dirs.push(root.to_path_buf());
    }
    let bin = root.join("bin");
    if bin.join(java_exe_name()).is_file() {
        bin_dirs.push(bin);
    }
}

/// [`push_java_root`] over a candidate root and its subfolders,
/// a few levels deep. Launchers nest their runtimes below the
/// candidate roots — HMCL keeps them at
/// `.hmcl\java\<platform>\<runtime>\bin`, a system JDK sits at
/// `Program Files\Java\<jdk>\bin` — and the core's recursive
/// `search_folders` (what `java list` runs) finds those, while a
/// root-only probe missed them (M2 real-machine run: launch saw
/// only the JAVA_HOME runtime).
fn collect_java_bins(root: &Path, depth: usize, bin_dirs: &mut Vec<PathBuf>) {
    push_java_root(root, bin_dirs);
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_java_bins(&path, depth - 1, bin_dirs);
        }
    }
}

fn discover_javas(extra: &[String]) -> Vec<FoundJava> {
    let mut bin_dirs: Vec<PathBuf> = Vec::new();
    for dir in extra {
        push_java_root(&PathBuf::from(dir), &mut bin_dirs);
    }
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        push_java_root(&PathBuf::from(java_home), &mut bin_dirs);
    }
    // PCL's candidate folders (Windows-shaped PCL strings).
    let env = JavaCandidateEnv {
        app_data: std::env::var("APPDATA").unwrap_or_default(),
        user_profile: std::env::var("USERPROFILE").unwrap_or_default(),
        local_app_data: std::env::var("LOCALAPPDATA").unwrap_or_default(),
        program_files: std::env::var("ProgramFiles").unwrap_or_default(),
        program_files_x86: std::env::var("ProgramFiles(x86)").unwrap_or_default(),
        my_documents: String::new(),
        jdk_home: std::env::var("JDK_HOME").ok(),
        java_home: std::env::var("JAVA_HOME").ok(),
        microsoft_jdk_dirs: vec![],
    };
    for folder in candidate_folders(&env) {
        collect_java_bins(&mcfix::to_native_path(&folder), 3, &mut bin_dirs);
    }
    if !cfg!(target_os = "windows") {
        if let Ok(entries) = std::fs::read_dir("/usr/lib/jvm") {
            for entry in entries.flatten() {
                push_java_root(&entry.path(), &mut bin_dirs);
            }
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let exe = PathBuf::from(dir).join("java");
                if exe.is_file() {
                    bin_dirs.push(PathBuf::from(dir));
                }
            }
        }
    }
    bin_dirs.sort();
    bin_dirs.dedup();
    bin_dirs.iter().filter_map(|d| probe_java_bin(d)).collect()
}

fn detect_mod_loaders(resolved: &Value) -> (ModLoaders, Option<String>, Option<String>, Option<String>) {
    let mut loaders = ModLoaders::NONE;
    let (mut forge, mut neoforge, mut fabric) = (None, None, None);
    if let Some(libraries) = resolved.get("libraries").and_then(Value::as_array) {
        for library in libraries {
            let name = library
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let parts: Vec<&str> = name.split(':').collect();
            let version_part = parts.get(2).copied().unwrap_or_default();
            if name.starts_with("net.minecraftforge:forge:") {
                loaders = loaders | ModLoaders::FORGE;
                // "1.20.1-47.1.0" → "47.1.0"
                forge = Some(
                    version_part
                        .split_once('-')
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_else(|| version_part.to_string()),
                );
            } else if name.starts_with("net.neoforged:neoforge:") {
                loaders = loaders | ModLoaders::NEOFORGE;
                neoforge = Some(version_part.to_string());
            } else if name.starts_with("net.fabricmc:fabric-loader:") {
                loaders = loaders | ModLoaders::FABRIC;
                fabric = Some(version_part.to_string());
            } else if name.starts_with("com.mumfrey:liteloader:") {
                loaders = loaders | ModLoaders::LITELOADER;
            }
        }
    }
    (loaders, forge, neoforge, fabric)
}

fn parse_release_time(resolved: &Value) -> (i32, u32, u32) {
    let text = resolved
        .get("releaseTime")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let date = text.get(..10).unwrap_or_default();
    let mut parts = date.split('-');
    let year = parts.next().and_then(|p| p.parse().ok()).unwrap_or(2013);
    let month = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    let day = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    (year, month, day)
}

// ---------------------------------------------------------------------------
// Account store persistence (CLI-level; PCL keeps these in its
// settings store — the port uses one JSON file).
// ---------------------------------------------------------------------------

fn store_path() -> PathBuf {
    if let Ok(path) = std::env::var("RERUST_PCL_STORE") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join(".rerust-pcl").join("store.json")
}

fn load_store() -> AccountStore {
    let path = store_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_store(store: &AccountStore) {
    let path = store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(store) {
        let _ = std::fs::write(&path, text);
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct LaunchState {
    launch_count: i64,
    hint_buy: bool,
}

fn state_path() -> PathBuf {
    store_path().with_file_name("launch-state.json")
}

fn load_state() -> LaunchState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_state(state: &LaunchState) {
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(state_path(), text);
    }
}

/// Whether the user's environment is Chinese, for the
/// precheck's buy-prompt gate. On Windows the answer comes from
/// the OS user locale (`GetUserDefaultLocaleName`, via the
/// core's platform hook): upstream PCL reads the Windows system
/// locale, while LC_ALL/LANG are normally unset under cmd —
/// reading only those misjudged a Chinese Windows install as
/// non-Chinese and hit the blocking demo gate (the M2
/// real-machine run had to fake LANG to get past it). Other
/// platforms keep the LC_ALL/LANG check, as before; the
/// environment is also the fallback if the OS query fails.
fn detect_chinese_locale() -> bool {
    if let Some(name) = platform::user_locale_name() {
        return name.to_lowercase().starts_with("zh");
    }
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    locale.to_lowercase().starts_with("zh")
}

// ---------------------------------------------------------------------------
// pcl-cli launch <instance>
// ---------------------------------------------------------------------------

pub fn cmd_launch(args: &[String]) -> ExitCode {
    let opts = Opts::parse(args);
    let Some(name) = opts.positional(0).map(str::to_string) else {
        eprintln!(
            "用法：pcl-cli launch <实例名> --user <玩家名> [--mc <目录>] [--ms] [--auth <服务器>] [--nide <服务器 id>] [--java-dir <目录>] [--ram-mb <MB>] [--dry-run] [--demo-gate buy|demo|cancel]"
        );
        return ExitCode::from(2);
    };
    match launch_inner(&opts, &name) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("启动失败：{e}");
            ExitCode::FAILURE
        }
    }
}

fn launch_inner(opts: &Opts, name: &str) -> Result<ExitCode, String> {
    let mc = opts
        .get("mc")
        .map(PathBuf::from)
        .unwrap_or_else(default_mc_dir);
    let isolate = opts.has("isolate");
    let indie = indie_dir(&mc, name, isolate);
    let version_path = mc.join("versions").join(name);
    let pref = source_preference(opts);
    let http = ReqwestClient::new();
    let mut store = load_store();
    let mut state = load_state();

    // ---- Login input (needed by the precheck, as upstream). ----
    let user = opts.get("user").unwrap_or_default().to_string();
    let (login_data, login_type, jvm_login) = if opts.has("ms") {
        let user_name = if user.is_empty() {
            store.ms_name.clone()
        } else {
            user.clone()
        };
        let refresh = store
            .ms_accounts()
            .ok()
            .and_then(|list| list.get(&user_name).map(str::to_string))
            .unwrap_or_else(|| store.ms_oauth_refresh.clone());
        let input = McLoginMs {
            user_name,
            oauth_refresh_token: refresh,
            ..McLoginMs::default()
        };
        (
            McLoginData::Ms(input),
            McLoginType::Ms,
            JvmLogin::None,
        )
    } else if let Some(server) = opts.get("auth") {
        let password = std::env::var("RERUST_PCL_PASSWORD")
            .map_err(|_| "第三方登录需要通过环境变量 RERUST_PCL_PASSWORD 提供密码".to_string())?;
        let input = McLoginServer::auth(&user, &password, server);
        (
            McLoginData::Server(input),
            McLoginType::Auth,
            JvmLogin::Auth {
                server: server.to_string(),
                prefetched_response: None,
            },
        )
    } else if let Some(server_id) = opts.get("nide") {
        let password = std::env::var("RERUST_PCL_PASSWORD")
            .map_err(|_| "第三方登录需要通过环境变量 RERUST_PCL_PASSWORD 提供密码".to_string())?;
        let input = McLoginServer::nide(&user, &password, server_id);
        (
            McLoginData::Server(input),
            McLoginType::Nide,
            JvmLogin::Nide {
                server_id: server_id.to_string(),
            },
        )
    } else {
        if user.is_empty() {
            return Err("离线登录需要 --user <玩家名>".to_string());
        }
        let input = McLoginLegacy {
            user_name: user.clone(),
            ..McLoginLegacy::default()
        };
        (McLoginData::Legacy(input), McLoginType::Legacy, JvmLogin::None)
    };

    // ---- Precheck (McLaunchPrecheck equivalent). ----
    let next_count = state.launch_count + 1;
    let outcome = precheck(&PrecheckInput {
        indie_path: native(&indie),
        version_path: native(&version_path),
        instance_error: None,
        login: login_data.clone(),
        login_type,
        hint_buy: state.hint_buy,
        launch_count: next_count,
        is_release_build: true,
        save_batch: false,
        is_chinese_locale: detect_chinese_locale(),
    })?;
    let mut extra_game_args: Vec<String> = Vec::new();
    match outcome {
        PrecheckOutcome::Proceed { prompts } => {
            for prompt in prompts {
                match prompt {
                    PrecheckPrompt::Donate => println!(
                        "提示：PCL 已经为你启动了 {next_count} 次游戏啦！如果 PCL 还算好用的话，能不能考虑赞助一下 PCL…… {DONATE_URL}"
                    ),
                    PrecheckPrompt::Buy => println!(
                        "提示：你已经启动了 {next_count} 次 Minecraft 啦！如果觉得 Minecraft 还不错，可以购买正版支持一下：{BUY_URL}"
                    ),
                }
            }
        }
        PrecheckOutcome::DemoGate => {
            let choice = match opts.get("demo-gate") {
                Some("buy") => DemoGateChoice::Buy,
                Some("demo") => DemoGateChoice::Demo,
                Some("cancel") => DemoGateChoice::Cancel,
                _ => {
                    return Err(
                        "你必须先登录正版账号，才能进行离线登录！请选择 --demo-gate buy（购买正版）/ demo（试玩）/ cancel（返回）"
                            .to_string(),
                    )
                }
            };
            if PrecheckOutcome::DemoGate.after_demo_gate(choice)? {
                extra_game_args.push("--demo".to_string());
                println!("游戏将以试玩模式启动！");
            }
        }
    }

    // ---- Login. ----
    let login_result: McLoginResult = match &login_data {
        McLoginData::Legacy(input) => login::legacy_login(input, false, false, &|_| {
            Err(login::LoginError::Message(
                "皮肤类型 3 需要正版 UUID 查询".to_string(),
            ))
        })
        .map_err(|e| e.to_string())?,
        McLoginData::Ms(input) => {
            let options = login::MsLoginOptions::default();
            if options.client_id.is_empty() {
                return Err(
                    "未配置微软 OAuth 客户端 ID（环境变量 PCL_MS_CLIENT_ID），无法进行正版登录"
                        .to_string(),
                );
            }
            let session = login::ms_login(
                &http,
                &options,
                input,
                &store,
                now_unix(),
                &mut |prompt| {
                    println!(
                        "请在浏览器打开 {} 并输入代码 {} 完成微软登录",
                        prompt.verification_uri, prompt.user_code
                    );
                },
            )
            .map_err(|e| e.to_string())?;
            // Persist the CacheMsV2* fields + account list entry.
            store.ms_oauth_refresh = session.oauth_refresh_token.clone();
            store.ms_access = session.result.access_token.clone();
            store.ms_uuid = session.result.uuid.clone();
            store.ms_name = session.result.name.clone();
            store.ms_profile_json = session.result.profile_json.clone();
            store.ms_expires = session.expires_unix;
            if let Ok(mut list) = store.ms_accounts() {
                list.record_login(
                    &input.user_name,
                    &session.result.name,
                    &session.oauth_refresh_token,
                );
                store.set_ms_accounts(&list);
            } else {
                let mut list = MsAccountList::default();
                list.record_login(
                    &input.user_name,
                    &session.result.name,
                    &session.oauth_refresh_token,
                );
                store.set_ms_accounts(&list);
            }
            save_store(&store);
            state.hint_buy = true;
            session.result
        }
        McLoginData::Server(input) => {
            let result =
                login::server_login(&http, input, &store).map_err(|e| e.to_string())?;
            // Persist the Cache{Token}* session fields.
            match input.token.as_str() {
                "Nide" => {
                    store.nide_access = result.access_token.clone();
                    store.nide_client = result.client_token.clone();
                    store.nide_uuid = result.uuid.clone();
                    store.nide_name = result.name.clone();
                    store.nide_user_name = input.user_name.clone();
                    store.nide_password = input.password.clone();
                }
                "Auth" => {
                    store.auth_access = result.access_token.clone();
                    store.auth_client = result.client_token.clone();
                    store.auth_uuid = result.uuid.clone();
                    store.auth_name = result.name.clone();
                    store.auth_user_name = input.user_name.clone();
                    store.auth_password = input.password.clone();
                }
                _ => {}
            }
            save_store(&store);
            result
        }
    };
    println!(
        "登录完成：{}（{}，{login_type:?}）",
        login_result.name, login_result.uuid
    );

    // ---- Fix files (DlClientFix). ----
    ensure_instance_files(&mc, name, pref, isolate, &http)?;

    // ---- Chain, libraries, natives. ----
    let items = build_chain(&mc, name)?;
    let resolved = &items[0].resolved;
    let base = items.last().unwrap();
    let mc_pcl = mcfix::mc_folder_pcl(&mc);
    let entries = lib_list_from_json_os(
        resolved,
        &mc_pcl,
        false,
        platform::host_is_64bit(),
        &platform::host_os_version(),
        platform::host_os_key(),
    )
    .map_err(|e| format!("分析支持库失败：{e}"))?;
    let natives_path = runner::natives_dir(&version_path, name);
    let natives_jars: Vec<PathBuf> = entries
        .iter()
        .filter(|e| e.is_natives)
        .map(|e| mcfix::to_native_path(&e.local_path))
        .collect();
    if !natives_jars.is_empty() {
        let extracted = runner::extract_natives(&natives_jars, &natives_path)?;
        println!("Natives 解压完成（新解压 {extracted} 个文件）");
    }

    // ---- Java selection. ----
    let (loaders, forge_v, neoforge_v, fabric_v) = detect_mod_loaders(resolved);
    let has_optifine = entries
        .iter()
        .any(|e| e.original_name.as_deref().unwrap_or_default().to_lowercase().contains("optifine"));
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
        mod_loaders: loaders,
        forge_version: forge_v,
        neoforge_version: neoforge_v,
        fabric_version: fabric_v,
        login_is_nide: login_type == McLoginType::Nide,
    });
    let extra_dirs: Vec<String> = opts.get("java-dir").map(|d| vec![d.to_string()]).unwrap_or_default();
    let javas = discover_javas(&extra_dirs);
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
            "没有符合要求的 Java：需要 {}，已发现：{found}。可通过 --java-dir 指定 Java 目录，或先安装对应版本（本版本不自动下载 Java）",
            describe_required_java(&requirement.range)
        ));
    };
    println!("使用 Java：{}（{}）", java.exe.display(), java.version);

    // ---- Argument assembly. ----
    let levels = chain_levels(&items);
    let os_version = platform::host_os_version();
    let jvm_input = JvmInput {
        os_version: os_version.clone(),
        custom_arg: String::new(),
        login: jvm_login,
        java_major: java.version.major,
        java_version: java.version,
        disable_jlw_global: false,
        disable_jlw_instance: false,
        is_gbk_encoding: false,
        jlw_jar_path: None,
        use_lua: false,
        lua_jar_path: None,
        ram_mb: opts
            .get("ram-mb")
            .and_then(|v| v.parse().ok())
            .unwrap_or(2048),
        gc_global: 4,
        gc_instance_override: None,
        os_major: 10,
        os_build: 0,
        mode_debug: false,
        file_encoding: java.file_encoding.clone(),
        native_encoding: java.native_encoding.clone(),
        fallback_encoding_name: "utf-8".to_string(),
    };
    let jvm_args = jvm_arguments(&levels, &jvm_input).map_err(|e| format!("生成 JVM 参数失败：{e}"))?;
    let mut game_args_vec = game_arguments(
        &levels,
        &GameInput {
            os_version,
            custom_arg: String::new(),
            extra_game_args,
            fullscreen: false,
            server: opts.get("server").map(str::to_string),
            release_time: parse_release_time(resolved),
            has_optifine,
            forge_or_liteloader: loaders.has(ModLoaders::FORGE) || loaders.has(ModLoaders::LITELOADER),
            optifine_as_mod: false,
        },
    );
    // Classpath: library entries + the base instance's main jar last.
    let mut cp_entries = entries.clone();
    cp_entries.push(LibEntry::new(
        Some("client".to_string()),
        format!(
            "{}{}",
            mc_pcl,
            format!("versions\\{}\\{}.jar", base.name, base.name)
        ),
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
        natives_directory: native(&natives_path),
        library_directory: native(&mc.join("libraries")),
        pure_directory: native(&mc),
        launcher_version: "2.99.0.1".to_string(),
        version_name: name.to_string(),
        game_directory: native(&indie),
        assets_root: native(&mc.join("assets")),
        game_assets: native(&mc.join("assets").join("virtual").join("legacy")),
        auth_player_name: login_result.name.clone(),
        auth_uuid: login_result.uuid.clone(),
        auth_access_token: login_result.access_token.clone(),
        primary_jar: native(&mcfix::primary_jar_path(&mc, &base.name)),
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

    if opts.has("dry-run") {
        println!("（dry-run）将要执行的命令：");
        println!("{} {}", java.exe.display(), display_args.join(" "));
        println!("工作目录：{}", indie.display());
        return Ok(ExitCode::SUCCESS);
    }

    // Unquote the display form for direct spawn (no shell involved).
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

    // ---- Spawn (McLaunchRun). ----
    state.launch_count = next_count;
    save_state(&state);
    let short: &dyn Fn(&str) -> String = if cfg!(target_os = "windows") {
        &pcl_core::pathutil::to_short_path
    } else {
        &|s: &str| s.to_string()
    };
    let mut child = runner::spawn_game(
        &runner::SpawnSpec {
            java_exe: java.exe.clone(),
            args: spawn_args,
            working_dir: indie.clone(),
            mc_folder: mc.clone(),
        },
        short,
    )?;
    println!("游戏进程已启动（PID {}）", child.id());
    // Forward the game's output until it exits.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        if let Some(mut pipe) = stdout.take() {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        use std::io::Write;
                        let _ = std::io::stdout().write_all(&buf[..n]);
                        let _ = std::io::stdout().flush();
                    }
                }
            }
        }
    });
    let err_thread = std::thread::spawn(move || {
        if let Some(mut pipe) = stderr.take() {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        use std::io::Write;
                        let _ = std::io::stderr().write_all(&buf[..n]);
                        let _ = std::io::stderr().flush();
                    }
                }
            }
        }
    });
    let status = child.wait().map_err(|e| format!("等待游戏进程失败：{e}"))?;
    let _ = out_thread.join();
    let _ = err_thread.join();
    println!("游戏进程已退出（{status}）");
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch-side discovery walk must find runtimes nested
    /// below a candidate root the way `java list`'s recursive
    /// search does (HMCL's `<platform>\<runtime>\bin` layout and
    /// `Program Files\Java\<jdk>\bin`), not just the root itself.
    #[test]
    fn collect_java_bins_finds_nested_runtimes() {
        let base = std::env::temp_dir().join(format!(
            "rerust-pcl-collect-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let exe = java_exe_name();
        // HMCL shape: root/<platform>/<runtime>/bin/<exe>.
        let hmcl_bin = base.join("hmcl").join("windows-x86_64").join("mojang-jre-legacy").join("bin");
        // JDK shape: root/<jdk>/bin/<exe>.
        let jdk_bin = base.join("Java").join("jdk-21").join("bin");
        // Direct shape: root/bin/<exe>.
        let direct_bin = base.join("direct").join("bin");
        for bin in [&hmcl_bin, &jdk_bin, &direct_bin] {
            std::fs::create_dir_all(bin).unwrap();
            std::fs::write(bin.join(exe), b"").unwrap();
        }
        // A folder tree without any runtime must contribute nothing.
        std::fs::create_dir_all(base.join("empty").join("nested")).unwrap();

        let mut bins = Vec::new();
        collect_java_bins(&base.join("hmcl"), 3, &mut bins);
        collect_java_bins(&base.join("Java"), 3, &mut bins);
        collect_java_bins(&base.join("direct"), 3, &mut bins);
        collect_java_bins(&base.join("empty"), 3, &mut bins);
        // A runtime's bin is seen twice by the walk (as a root's
        // `bin` and as a walked folder holding the exe itself);
        // `discover_javas` dedups, and so does this test.
        bins.sort();
        bins.dedup();
        assert!(bins.contains(&hmcl_bin), "HMCL-nested runtime missed: {bins:?}");
        assert!(bins.contains(&jdk_bin), "JDK-nested runtime missed: {bins:?}");
        assert!(bins.contains(&direct_bin), "direct runtime missed: {bins:?}");
        assert_eq!(bins.len(), 3, "unexpected extra bins: {bins:?}");

        // Depth limiting: with depth 0 only the root itself counts.
        let mut shallow = Vec::new();
        collect_java_bins(&base.join("hmcl"), 0, &mut shallow);
        assert!(shallow.is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }
}
