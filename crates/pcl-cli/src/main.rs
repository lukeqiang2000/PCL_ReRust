//! pcl-cli — command-line front end for pcl-core.
//!
//! Subcommands:
//!   inspect <version.json>    parse one version JSON (phase-1 behavior)
//!   name-map <version-name>   map a version JSON `id` to its vanilla name
//!   scan <versions-dir>       list installed instances, resolving chains
//!   java list [dir ...]       discover Java runtimes (candidate folders
//!                             plus any extra directories given)

mod m2;

use pcl_core::instance::scan_instances;
use pcl_core::java::{candidate_folders, search_folders, JavaCandidateEnv, ProcessJavaRunner};
use pcl_core::version::name_to_version;
use std::path::PathBuf;
use std::process::ExitCode;

fn inspect(path: &str) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match pcl_core::version::VersionJson::parse(&text) {
        Ok(v) => {
            let cmp = v.comparable_version();
            println!("id:            {}", v.id);
            println!("type:          {}", v.version_type);
            println!("inherits_from: {}", v.inherits_from.as_deref().unwrap_or("-"));
            println!("libraries:     {}", v.libraries.len());
            println!(
                "comparable:    {}.{}.{} (drop {})",
                cmp.major,
                cmp.minor,
                cmp.patch,
                cmp.drop_ordinal()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot parse {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("inspect") => match args.get(1) {
            Some(path) => inspect(path),
            None => {
                eprintln!("usage: pcl-cli inspect <path-to-version.json>");
                ExitCode::from(2)
            }
        },
        Some("name-map") => match args.get(1) {
            Some(name) => {
                let cmp = name_to_version(name);
                println!(
                    "{}.{}.{} (drop {})",
                    cmp.major,
                    cmp.minor,
                    cmp.patch,
                    cmp.drop_ordinal()
                );
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: pcl-cli name-map <version-name>");
                ExitCode::from(2)
            }
        },
        Some("scan") => match args.get(1) {
            Some(dir) => {
                let found = scan_instances(&PathBuf::from(dir));
                if found.is_empty() {
                    println!("(no instances found)");
                }
                for inst in found {
                    println!(
                        "{}: id={} inherits={} mainClass={} libraries={}",
                        inst.folder_name,
                        inst.json_id.as_deref().unwrap_or("-"),
                        inst.inherits_from.as_deref().unwrap_or("-"),
                        if inst.main_class.is_empty() { "-" } else { &inst.main_class },
                        inst.library_count,
                    );
                    for warning in &inst.warnings {
                        println!("  ! {warning}");
                    }
                }
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("usage: pcl-cli scan <versions-dir>");
                ExitCode::from(2)
            }
        },
        Some("versions") => m2::cmd_versions(&args[1..]),
        Some("install") => m2::cmd_install(&args[1..]),
        Some("launch") => m2::cmd_launch(&args[1..]),
        Some("java") if args.get(1).map(String::as_str) == Some("list") => {
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
            let candidates = candidate_folders(&env);
            let mut roots = candidates.clone();
            roots.extend(args.iter().skip(2).cloned());
            let runner = ProcessJavaRunner;
            let javas = search_folders(&roots, true, None, &candidates, &runner);
            if javas.is_empty() {
                println!("(no Java runtimes found)");
            }
            for java in javas {
                println!("{}", java.display());
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "usage: pcl-cli <inspect|name-map|scan|java list|versions|install|launch> ..."
            );
            ExitCode::from(2)
        }
    }
}
