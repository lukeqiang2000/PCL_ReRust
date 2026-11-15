//! pcl-core — core logic of the PCL (Plain Craft Launcher) Rust port.
//!
//! Platform-neutral core library (Windows / macOS / Linux; see
//! PORT-PLAN.md §8 for the cross-platform design constraints).
//! Modules port the Minecraft version JSON model and version-name
//! semantics ([`version`], PCLCS McVersion / ModMinecraft), path and
//! range utilities ([`pathutil`], [`vrange`], MeloongCore), Java
//! discovery and requirements ([`java`], ModJava), instance
//! inheritance and launch argument assembly ([`instance`],
//! [`launch`], ModMinecraft / ModLaunch), and the account data
//! model ([`account`], ModLaunch login data + account settings).

pub mod account;
pub mod download;
pub mod http;
pub mod instance;
pub mod java;
pub mod launch;
pub mod login;
pub mod manifest;
pub mod mcfix;
pub mod platform;
pub mod precheck;
pub mod runner;
pub mod pathutil;
pub mod version;
pub mod vrange;
