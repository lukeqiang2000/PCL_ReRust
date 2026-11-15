//! Process execution: natives extraction and the game spawn.
//!
//! Ports of `ModLaunch.vb`'s `McLaunchNatives` and `McLaunchRun`:
//! - Natives: every natives jar contributes its native-library
//!   entries (`.dll` upstream; the host extension here —
//!   dll/so/dylib) into `<version>/<name>-natives`; entries whose
//!   file already exists with the same size are skipped, files in
//!   the folder that no jar provides are deleted, and a corrupt
//!   jar is removed with an error.
//! - Run: the Java executable is started directly (not javaw —
//!   upstream #6263), working directory = the instance's indie
//!   dir, stdout/stderr redirected for log passthrough, the Java
//!   folder appended to the PATH environment variable (upstream
//!   appends, deduplicated), and `appdata` set to the .minecraft
//!   folder. Paths pass through the caller's short-path transform
//!   (identity off Windows).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::platform;

/// The natives folder of an instance: `<version_path>/<name>-natives`
/// (upstream `GetNativesFolder`; its non-ASCII/GBK temp fallbacks
/// are Windows-console concerns and are not reproduced).
pub fn natives_dir(version_path: &Path, instance_name: &str) -> PathBuf {
    version_path.join(format!("{instance_name}-natives"))
}

/// Extract natives from `jars` into `dir` (`McLaunchNatives`).
/// Returns the number of files extracted (skips not counted).
pub fn extract_natives(jars: &[PathBuf], dir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 Natives 文件夹失败：{e}"))?;
    let extension = platform::natives_extension();
    // The expected content: file name → size.
    let mut expected: Vec<(String, u64)> = Vec::new();
    let mut extracted = 0usize;
    for jar in jars {
        let file = std::fs::File::open(jar)
            .map_err(|e| format!("打开 Natives 文件失败（{}）：{e}", jar.display()))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| {
            // Upstream deletes a natives jar it cannot read, then
            // fails — the file will be re-downloaded next launch.
            let _ = std::fs::remove_file(jar);
            format!("Natives 文件损坏，已删除（{}）：{e}", jar.display())
        })?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("读取 Natives 条目失败：{e}"))?;
            if entry.is_dir() {
                continue;
            }
            let entry_name = entry.name().replace('\\', "/");
            let file_name = entry_name.rsplit('/').next().unwrap_or_default().to_string();
            if !file_name.to_lowercase().ends_with(&format!(".{extension}")) {
                continue;
            }
            let size = entry.size();
            expected.push((file_name.clone(), size));
            let target = dir.join(&file_name);
            if let Ok(meta) = std::fs::metadata(&target) {
                if meta.len() == size {
                    continue; // same size → already extracted
                }
            }
            let mut bytes = Vec::with_capacity(size as usize);
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| format!("解压 Natives 文件失败（{file_name}）：{e}"))?;
            std::fs::write(&target, &bytes)
                .map_err(|e| format!("写入 Natives 文件失败（{file_name}）：{e}"))?;
            extracted += 1;
        }
    }
    // Delete leftovers no jar provides (upstream behavior).
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
            if !expected.iter().any(|(n, s)| *n == name && *s == size) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(extracted)
}

/// Inputs for [`spawn_game`], mirroring `McLaunchRun`'s start info.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// The Java executable (upstream uses the exe path directly,
    /// never javaw).
    pub java_exe: PathBuf,
    /// The argument vector (quoting is applied by the OS layer;
    /// `launch::assemble_launch_arguments` produces the display
    /// form of the same list).
    pub args: Vec<String>,
    /// Working directory = the instance's indie dir.
    pub working_dir: PathBuf,
    /// The .minecraft folder, exported as `appdata`.
    pub mc_folder: PathBuf,
}

/// Start the game process (`McLaunchRun`). Stdout and stderr are
/// piped so the caller can forward them to the launch log; the
/// returned child owns them.
pub fn spawn_game(spec: &SpawnSpec, short_path: &dyn Fn(&str) -> String) -> Result<Child, String> {
    let java_folder = spec
        .java_exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let path_key = if cfg!(target_os = "windows") {
        "Path"
    } else {
        "PATH"
    };
    let separator = if cfg!(target_os = "windows") { ";" } else { ":" };
    let mut paths: Vec<String> = std::env::var(path_key)
        .unwrap_or_default()
        .split(separator)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let java_folder_short = short_path(&java_folder.to_string_lossy());
    if !paths.contains(&java_folder_short) {
        paths.push(java_folder_short);
    }
    let mut command = Command::new(&spec.java_exe);
    command
        .args(&spec.args)
        .current_dir(&spec.working_dir)
        .env(path_key, paths.join(separator))
        .env(
            "appdata",
            short_path(&spec.mc_folder.to_string_lossy()),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .spawn()
        .map_err(|e| format!("启动游戏进程失败：{e}"))
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn make_natives_jar(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, data) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn natives_extraction_filters_and_cleans() {
        let dir = std::env::temp_dir().join(format!(
            "rerust-natives-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ext = platform::natives_extension();
        let jar = dir.join("natives.jar");
        make_natives_jar(
            &jar,
            &[
                (&format!("libfoo.{ext}"), b"native-bytes"),
                ("notes.txt", b"not a native library"),
                (&format!("sub/libbar.{ext}"), b"bar"),
            ],
        );
        let out = dir.join("1.20.1-natives");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("stale.txt"), b"leftover").unwrap();
        let extracted = extract_natives(&[jar], &out).unwrap();
        assert_eq!(extracted, 2);
        assert_eq!(
            std::fs::read(out.join(format!("libfoo.{ext}"))).unwrap(),
            b"native-bytes"
        );
        assert!(out.join(format!("libbar.{ext}")).is_file(), "entries are flattened");
        assert!(!out.join("notes.txt").exists(), "non-native entries skipped");
        assert!(!out.join("stale.txt").exists(), "leftovers are deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_reports_exit_status() {
        let dir = std::env::temp_dir().join(format!("rerust-spawn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = |s: &str| s.to_string();
        let mut child = spawn_game(
            &SpawnSpec {
                java_exe: PathBuf::from("/bin/true"),
                args: vec![],
                working_dir: dir.clone(),
                mc_folder: dir.clone(),
            },
            &identity,
        )
        .unwrap();
        assert!(child.wait().unwrap().success());
        let mut child = spawn_game(
            &SpawnSpec {
                java_exe: PathBuf::from("/bin/false"),
                args: vec![],
                working_dir: dir.clone(),
                mc_folder: dir.clone(),
            },
            &identity,
        )
        .unwrap();
        assert!(!child.wait().unwrap().success());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
