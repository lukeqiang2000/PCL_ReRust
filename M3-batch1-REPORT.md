# M3 第一批报告：Slint 图形界面骨架（pcl-ui）

日期：2026-11-15。工程/仓库名 **PCL_ReRust**，应用显示名 **PCL ReRust**（用户 2026-11-15 定）。本批不发布、不传 PC（GitHub 推送除外，见 §5）。

## 1. 原版 UI 结构清单（复刻对象）

研读对象：`upstream/Plain Craft Launcher 2/` 的 `FormMain.xaml`、各 `Pages/` XAML。复刻布局与功能组织，不复制原版图标/图片素材（许可）。

**主窗体 FormMain**：850×500（最小 810×470），无边框自绘窗口、圆角 6、带四边拖拽缩放与开场动画。

- **标题栏 PanTitle（高 48，主题蓝渐变）**：左侧 PCL 描边 Logo（或自定义标题图/文字）；**中部一级导航**（MyRadioButton ×5）：启动（0）、下载（1）、联机（2）、设置（3）、更多（4）；右侧最小化/关闭按钮（28×28）。子页面时切换为 PanTitleInner：返回按钮＋页面标题。
- **内容区**：左侧半透明边栏（PanLeft，各页自带）＋主区 PanMain（PanMainLeft＋PanMainRight 双栏）——每个一级页面都是一对 Left/Right 控件。
- **浮动件**：右下角按钮栈（返回顶部、任务管理、关闭 Minecraft、音乐，按状态显隐）；左下角提示栈 PanHint；消息层 PanMsg。

**启动页（主页）**：

- `PageLaunchLeft`（宽 300）：当前版本提示 chip；**登录区**——登录方式单选（正版 Ms=5／离线 Legacy=0／统一通行证 Nide=2，另有 authlib 动态项），中部嵌入对应登录页（PageLoginMs/Legacy/Nide/Auth 及皮肤页）；**大启动按钮 BtnLaunch（高 54，强调色）**，其下 `LabVersion`（11px 灰字显示当前版本名）；底部「版本选择」＋「版本设置」按钮（高 35）。启动中显示覆盖层：步骤列表、总进度、下载速度、「你知道吗」提示、取消按钮。
- `PageLaunchRight`：公告区（PanHint，含快照版提示）、自定义按钮区（PanCustom）、日志区（PanLog）。

**其余一级页面**：

- 版本选择：PageSelectLeft/Right（文件夹＋版本列表）；测速/任务：PageSpeedLeft/Right。
- 下载：PageDownloadLeft（分类导航）＋ PageDownloadInstall（资源详情与版本选择安装）＋ Resource 列表项（整合包/Mod/资源包/地图等）。
- 实例：PageInstanceLeft ＋ Overall/Setup/Mod/ModDisabled/Export 子页。
- 联机：PageLinkMain。
- 设置：PageSetupLeft ＋ Launch/UI/Link/System 四个子页。
- 更多：PageOtherLeft ＋ About（作者署名、赞助者名单、检查更新、赞助 PCL 等）/Help/HelpDetail/Test。

**主题色（Application.xaml 的 ColorObject）**：文字深灰 `#343d4a`、深蓝 `#0b5bcb`、强调蓝 `#1370f3`、标题栏蓝 `#4890f5`、浅蓝 `#96c0f9` / `#d5e6fd` / `#e0eafd`、页底 `#eaf2fe`。主色相为蓝色（许可硬要求）。

## 2. 本批完成项

**新 crate `crates/pcl-ui`**（Slint 1.18，无 WebView），已加入 workspace：

- **主窗口骨架**（`ui/app.slint`，窗口标题「PCL ReRust」，850×500、最小 810×470）：48px 蓝色标题栏 `#4890f5`——左文字 Logo（不复制原版描边素材）、中部一级导航五项（启动/下载/联机/设置/更多，可切换）、右最小化/关闭（已接线）。页底 `#eaf2fe`、强调色 `#1370f3`，与原版色板一致。
- **启动页完整骨架**：左侧 300px 启动列（当前实例名、登录方式三单选、离线玩家名输入、54px 大启动按钮、版本信息行、版本选择/版本设置按钮）；右侧实例列表＋启动日志＋状态行。
  - 与原版的已知差异（有意为之）：原版「版本选择」是独立页，本批把实例列表直接放主页右侧以先打通接线，独立选择页留下批；原版右侧的公告区暂缺（日志区先行）。
- **其余一级页面占位**：下载、联机、设置三页为占位（注明后续复刻对象与子页结构）；「更多」页含**关于区：首位署名原作者龙腾猫跃＋赞助链接（meloong.com/afd/a/LTCat）＋第三方独立二次创作声明**（许可预埋）。
- **接线 pcl-core**（`src/controller.rs`）：
  - 实例扫描：平台候选目录找 `.minecraft` → `instance::scan_instances` → 进右侧列表显示（名称、版本 id、库数量、继承关系）。
  - 启动链：选实例＋离线玩家名 → 点启动 → `precheck`（`McLaunchPrecheck` 等价，赞助/正版提示以日志行呈现）→ `login::legacy_login`（离线 UUID）→ 继承链解析 → OS 库列表 → natives 解压 → Java 需求计算与本机发现 → 参数拼装（JVM/游戏参数＋占位符替换＋classpath）→ `runner::spawn_game`。runner 入口经 `Spawner` trait 抽象，生产实现真拉进程（子线程排空输出），测试用 RecordingSpawner 记录 SpawnSpec 验证调用链。
  - 范围切割（日志中明示、不藏）：UI 侧未接下载补齐，缺文件时明确报错并指引先用 `pcl-cli install`；仅离线登录接通，正版/统一通行证在界面上点选会提示未接入；非中文环境的正版购买门（DemoGate）以错误信息停链，弹窗留下批；启动计数为内存计数（提示节奏与 core 的 precheck 一致）。
- **跨平台**：UI 层平台事实集中在 `src/platform.rs`（`.minecraft` 候选、Java 搜索根、可执行名，全部 cfg 分发）；controller 只消费 PathBuf，无平台假设。
- **根目录文件**：`README.md`（名称 PCL_ReRust／显示名 PCL ReRust、署名与许可说明、构建法）、`.gitignore`、`build-macos.sh`、`.github/workflows/build.yml`（后两者状态见 §5）。

## 3. 构建与测试结果

| 项 | 结果 |
|---|---|
| `cargo build`（Linux，debug） | 通过，0 error 0 warning |
| `cargo test --workspace` | **100/100 通过**（pcl-core 95＋pcl-cli 1＋pcl-ui 4；UI 的 4 个含「假 JDK＋假实例夹具→启动链直达 Spawner」端到端单测） |
| Linux release | `target/release/pcl-ui` **33,808,032 字节**；`target/release/pcl-cli` 8,631,248 字节 |
| Windows release（cargo-xwin，x86_64-pc-windows-msvc） | `target/x86_64-pc-windows-msvc/release/pcl-ui.exe` **16,526,848 字节**；`pcl-cli.exe` 4,932,096 字节。exe 文件名保持 `pcl-ui.exe`（用户定不必改）。构建时有 1 个 dead_code 警告（`platform::home_dir` 在 Windows 下未用），已用 cfg 收敛修复，修复后本机 `cargo check` 0 警告；Windows 侧未为此单独重编（改动只是给未用函数加 cfg 门） |
| macOS check | **未通过，卡点在环境不在代码**：`cargo check --target x86_64-apple-darwin`（target 已 rustup 安装）停在依赖 `ring 0.17.14`（reqwest 的 rustls 后端）的构建脚本——它用宿主 `cc` 带 `-arch`、`-mmacosx-version-min` 等 Apple 参数编译 C 代码，本机只有 GCC、无 clang、无 macOS SDK、无 osxcross，无法在本 VM 推进。真实 macOS 构建走 macos runner（见 §5）或在 Mac 上跑 `build-macos.sh` |

## 4. macOS 交付（脚本级）

- `build-macos.sh`：`cargo build --release` 后组装 `dist/PCL_ReRust.app`（Info.plist：显示名 PCL ReRust、Bundle ID `com.github.lukeqiang2000.pclrerust`、含 pcl-ui 与 pcl-cli），有 codesign 时做 ad-hoc 签名；**未做公证**，脚本头已注明公开分发需 Developer ID＋公证与许可命名复核。
- `.github/workflows/build.yml`：三 runner（windows-latest、macos-latest、ubuntu-latest）构建＋测试，Linux 装 GUI 系统库，macOS 步调用 `build-macos.sh` 并上传 .app，三平台二进制分别上传 Artifact。**该文件尚未进仓库**，原因见 §5。

## 5. GitHub 推送状态

- 已推送：https://github.com/lukeqiang2000/PCL_ReRust（公开，main＝`efac9ff`，392 个文件）。`upstream/` 整体纳入（21MB，最大单文件 2.67MB 的「最新正式版.zip」，无 >100MB 文件，不会被拒收）；`target/` 已由 .gitignore 排除；代码与脚本经扫描无 token/凭据/本机隐私路径。
- **Actions 未触发（明确卡点）**：当前 gh 的 OAuth 令牌缺 `workflow` 权限范围，GitHub 拒绝一切包含 workflow 文件的推送。因此：① 本仓 `.github/workflows/build.yml` 已写好但只能留在本地工作区；② `upstream/.github/workflows/build.yml`（上游自带）同因未纳入仓库（其余 upstream 文件齐全），两处都在 .gitignore 里写明原因。**解法**：用户执行一次 `gh auth refresh -h github.com -s workflow`（设备码授权）后，补推这两个文件，Actions 即会三平台自动构建——在那之前，三平台结论以 §3 的本地证据为准（Linux、Windows 有真二进制；macOS 为检查级卡点），不要当作 Actions 已验证。

## 6. 未做项与下一批建议

1. **UI 实体页**：独立版本选择页（PageSelect）、下载页（分类＋资源详情/安装）、设置页四子页、实例页（总览/设置/Mod 管理）；无边框自绘窗口（圆角、拖拽缩放、开场动画）与原版自绘控件质感。
2. **启动体验**：启动中进度覆盖层（步骤/进度/速度/取消）、游戏输出实时回显到日志区、正版购买门与赞助提示改为弹窗、启动计数与设置持久化。
3. **登录**：微软登录 UI（设备码流，core 已备）、yggdrasil（Auth/Nide）登录页与皮肤页。
4. **接线补全**：UI 侧下载补齐（DlClientFix 两阶段）、Mod 加载器检测接入 Java 需求（现以 ModLoaders(0) 近似，Forge/Fabric 实例的 Java 范围可能不精确）、HMCL Java 托管目录的深度发现已在候选根内但未经真机验。
5. **平台**：macOS 真机构建与签名/公证（待 Mac 或 Actions 工作流补推后）；Windows/macOS 上 GUI 的实际打开验证（本批仅构建级，VM 无显示环境、未传 PC）。
6. **许可待办（发布前复核）**：① 显示名已按用户定为「PCL ReRust」，而许可要求公开发布名以 `Plain Craft Launcher (PCL)` 开头加后缀——正式对外发布前需就该命名复核一次；② Slint 授权（royalty-free 桌面条款）与 PCL 分发许可的兼容复核；③ 关于页署名已预埋，发布版需再核对赞助链接与声明文案与许可原文一致。
