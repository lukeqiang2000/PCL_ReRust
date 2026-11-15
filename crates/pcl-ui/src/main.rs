//! ReRust_PCL 图形界面入口（M3 第一批）。
//!
//! 窗口骨架复刻 PCL 原版组织（标题栏一级导航 + 启动页左右分栏），
//! 数据与启动链路全部接 `pcl-core`：实例扫描进版本选择区，
//! 启动按钮走 controller 的 precheck → 离线登录 → 参数拼装 →
//! runner 调用链。界面渲染使用 Slint，无 WebView。

mod controller;
mod platform;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::{ModelRc, SharedString, VecModel};

use controller::{launch_flow, scan_dir, scan_workspace, ProcessSpawner};
use pcl_core::instance::InstanceSummary;

slint::include_modules!();

struct UiState {
    mc_dir: PathBuf,
    instances: Vec<InstanceSummary>,
    launch_count: i64,
    log: Vec<String>,
}

fn instance_model(instances: &[InstanceSummary]) -> ModelRc<InstanceItem> {
    let items: Vec<InstanceItem> = instances
        .iter()
        .map(|inst| {
            let id = inst.json_id.clone().unwrap_or_else(|| inst.folder_name.clone());
            let mut detail = format!("{id} · {} 个库", inst.library_count);
            if let Some(parent) = &inst.inherits_from {
                detail.push_str(&format!(" · 继承自 {parent}"));
            }
            if !inst.warnings.is_empty() {
                detail.push_str(" · 有警告");
            }
            InstanceItem {
                name: SharedString::from(inst.folder_name.as_str()),
                detail: SharedString::from(detail.as_str()),
            }
        })
        .collect();
    ModelRc::new(VecModel::from(items))
}

fn push_log(window: &AppWindow, state: &mut UiState, lines: impl IntoIterator<Item = String>) {
    state.log.extend(lines);
    let model: Vec<SharedString> = state.log.iter().map(SharedString::from).collect();
    window.set_log_lines(ModelRc::new(VecModel::from(model)));
    if let Some(last) = state.log.last() {
        window.set_status_line(SharedString::from(last.as_str()));
    }
}

fn refresh(window: &AppWindow, state: &Rc<RefCell<UiState>>) {
    let mut state = state.borrow_mut();
    state.instances = scan_dir(&state.mc_dir);
    let count = state.instances.len();
    window.set_instances(instance_model(&state.instances));
    window.set_mc_dir_label(SharedString::from(state.mc_dir.to_string_lossy().as_ref()));
    if window.get_selected_instance() < 0 && count > 0 {
        window.set_selected_instance(0);
    }
    let message = format!("已扫描 {}：{count} 个实例", state.mc_dir.display());
    push_log(window, &mut state, [message]);
}

fn main() -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;

    let workspace = scan_workspace();
    let state = Rc::new(RefCell::new(UiState {
        mc_dir: workspace.mc_dir,
        instances: workspace.instances,
        launch_count: 0,
        log: Vec::new(),
    }));

    refresh(&window, &state);

    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_refresh_instances(move || {
            if let Some(window) = weak.upgrade() {
                refresh(&window, &state);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_placeholder(move |text| {
            if let Some(window) = weak.upgrade() {
                window.set_status_line(text);
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_launch_clicked(move || {
            let Some(window) = weak.upgrade() else { return };
            if window.get_login_type() != 0 {
                window.set_status_line(SharedString::from(
                    "该登录方式将在后续批次接入，本批请使用离线登录",
                ));
                return;
            }
            let idx = window.get_selected_instance();
            let mut state = state.borrow_mut();
            let Some(instance) = state.instances.get(idx as usize) else {
                push_log(&window, &mut state, ["请先在右侧列表选择一个实例".to_string()]);
                return;
            };
            let name = instance.folder_name.clone();
            let player = window.get_player_name().to_string();
            state.launch_count += 1;
            let count = state.launch_count;
            push_log(&window, &mut state, [format!("开始启动 {name} ……")]);
            match launch_flow(&state.mc_dir.clone(), &name, &player, count, &ProcessSpawner) {
                Ok(lines) => push_log(&window, &mut state, lines),
                Err(e) => push_log(&window, &mut state, [format!("启动失败：{e}")]),
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_minimize_app(move || {
            if let Some(window) = weak.upgrade() {
                window.window().set_minimized(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_quit_app(move || {
            if let Some(window) = weak.upgrade() {
                window.hide().ok();
            }
            let _ = slint::quit_event_loop();
        });
    }

    window.run()
}
