# PCL_ReRust —— PCL ReRust

用 Rust 移植 [Plain Craft Launcher（PCL）](https://github.com/Meloong-Git/PCL) 的第三方独立二次创作。启动器显示名 **PCL ReRust**；核心逻辑（版本/下载/登录/启动）在 `crates/pcl-core`，命令行前端在 `crates/pcl-cli`，图形界面（Slint，无 WebView）在 `crates/pcl-ui`。同时支持 Windows、macOS、Linux，平台差异走 cfg/抽象层。

> 本项目是第三方作品，与 Meloong 官方无关。原作作者：**龙腾猫跃（Meloong）**，赞助原作：https://meloong.com/afd/a/LTCat
>
> 本仓库包含 PCL 上游源码（`upstream/`，版本 2.99.0.1）作为移植参照，其权利归原作者所有；本移植遵循《PCL 分发有限许可》与《PCL 存储库合理使用指南》（见 `upstream/LICENCE`）：公开源代码、关于页首位署名原作者、启动流程实现与 `McLaunchPrecheck` 一致的正版购买劝导与赞助劝导、Windows 界面主色相为蓝色。移植计划与进度见 [PORT-PLAN.md](PORT-PLAN.md)。

## 构建

需要 stable Rust 工具链。

```sh
cargo build --release --workspace   # 全部
cargo test --workspace              # 全部测试
```

- Windows：`target/release/pcl-ui.exe`（图形界面）、`target/release/pcl-cli.exe`（命令行）。
- Linux：`target/release/pcl-ui`、`target/release/pcl-cli`（构建机需 X11/Wayland 开发库，如 `libxkbcommon-dev`）。
- macOS：在 Mac 上运行 `./build-macos.sh`，产出 `dist/PCL_ReRust.app`。

三平台构建由 GitHub Actions 自动执行（`.github/workflows/build.yml`），产物在每次运行的 Artifacts 中。

## 命令行用法速览

```sh
pcl-cli versions                     # 版本清单与最新版
pcl-cli install <版本>               # 下载并补齐一个原版版本
pcl-cli launch <实例> --user <玩家名>  # 离线启动（更多登录方式见 --help）
pcl-cli scan                         # 扫描现成 .minecraft 的实例
```
