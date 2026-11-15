# ReRust_PCL：PCL → Rust 移植计划（第一阶段：盘点与计划）

- 平台与 UI 定案（2026-11-14 用户）：同时支持 Windows、macOS、Linux 三大系统；不用 WebView；**UI 框架定为 Slint**（原生 GPU 渲染）。核心逻辑保持平台中立，平台差异走 cfg/抽象层。

- 项目名：**ReRust_PCL**（用户 2026-11-14 15:07 在 Dolphin AI 定名；注意许可证要求公开发布时的名称须以 `Plain Craft Launcher (PCL)` 开头加第三方后缀，`ReRust_PCL` 作为内部工程名，公开命名须另按许可证口径处理，见 §3）。

- 对象：Meloong-Git/PCL（Plain Craft Launcher），HEAD `93ede5d9e638c99fcec853a9ba1eecd91ceee7f5`
  - 提交标题「2.99.0.1 正式版」，2026-10-24；REA 从仓库自带正式版 exe 读到的程序集版本同为 **2.99.0.1**，互相印证。
  - 源码在 `upstream/`（codeload tarball 解压，约 20MB / 293 文件；`MeloongCore` 为 git 子模块，tarball 不含，已按其仓库 HEAD `7655fe90` 单独补齐到该目录）。
- **先纠正一个前提**：PCL 主程序不是 C#，而是 **VB.NET**（.vb）+ WPF/XAML，目标框架 .NET Framework 4.8；C# 只有两个类库（PCLCS 与子模块 MeloongCore）。移植的源语言主体是 VB.NET。

## 1. 代码盘点

| 部分 | 文件 | 行数 | 说明 |
|---|---|---|---|
| 主程序 `Plain Craft Launcher 2/`（VB.NET） | 113 × .vb | 43,128 | WinExe、WPF、旧式 .vbproj（非 SDK 风格） |
| XAML 界面 | 63 × .xaml | 6,511 | 与 .vb code-behind 一一对应 |
| `PCLCS/`（C#，net48 类库） | 9 × .cs | 920 | 新式核心逻辑的落点（版本语义、启动工具、Java 封装） |
| `MeloongCore/`（C# 子模块） | ~50 × .cs | 5,812 | 作者自用基础库（Shared 工具 + Wpf 帮助类），主程序与 PCLCS 都依赖它 |
| 合计源码 | | **约 56,400 行** | 另有 48 png、10 dll、4 jar 等二进制资源 |

### 模块地图（主程序内）

| 领域 | 位置 | 规模 | 内容 |
|---|---|---|---|
| 启动核心 | `Modules/Minecraft/ModLaunch.vb` | 2,221 行 | 启动全流程编排、`McLaunchPrecheck`（正版/赞助劝导，许可证点名要求一致实现）、参数拼装、进程拉起 |
| 版本/实例 | `Modules/Minecraft/ModMinecraft.vb` | 2,153 行 | 版本扫描、实例管理、版本 JSON 消费 |
| 下载引擎 | `Modules/Base/ModNet.vb` + `Modules/Minecraft/ModDownload.vb` | 1,933 + 1,388 行 | 多源下载、断点/校验、任务调度 |
| 资源下载 | `Pages/PageDownload/ModDownloadLib.vb` 等 | 2,176 行起 | 整合包/Mod 等资源的检索与安装（`PageDownloadInstall` 1,199 行） |
| Java 发现 | `Modules/Minecraft/ModJava.vb` + `PCLCS/Java.cs` | 584 行 + C# 部分 | Java 运行时搜索、版本判定、自动下载衔接 |
| 崩溃分析 | `Modules/Minecraft/ModCrash.vb` | 1,122 行 | 游戏崩溃报告解析与提示 |
| 整合包 | `Modules/Minecraft/ModModpack.vb` | 866 行 | 整合包导入/导出 |
| 账号/登录 | `Pages/PageLaunch/PageLogin*.xaml(.vb)` | 4 套登录页 | 微软登录（Ms）、正版/authlib（Auth）、旧式（Legacy）、统一通行证（Nide）；皮肤配套页 |
| 基础层 | `Modules/Base/`（ModBase 1,216、ModLoader、ModValidate、ModAnimation 1,066、PclLogger、MyBitmap） | 约 5,000 行 | 配置、加载器任务框架、校验、动画、日志 |
| UI 层 | `Pages/`（Launch/Download/Instance/Link/Setup/Other）+ `Controls/`（6,409 行 .vb）+ `FormMain.xaml.vb`（1,735 行） | 约 28,000 行 | 全部 WPF 页面与自绘控件——移植时整体重写，不逐行翻译 |
| C# 核心 | `PCLCS/Minecraft/McVersion.cs`、`McInstallManifest.cs`、`MCUtils.cs`、`Launch/LaunchUtils.cs`、`Configs.cs` | 920 行 | 版本名语义（已作样板移植，见 §5）、安装清单、启动工具 |

### 第三方依赖

- 主程序（.vbproj 的本地 DLL 引用，无 NuGet 还原）：Newtonsoft.Json 13.0、NAudio 1.7.3、Imazen.WebP 10.0.1、CacheCow 2.13.1（Client/Common/FileStore）、Ookii.Dialogs.Wpf、ThrottleDebounce 2.0.2；框架引用含 WPF 全家桶、System.Management、System.Windows.Forms 等。
- PCLCS（唯一 PackageReference）：PolySharp 1.15.0（语法糖 polyfill）；Newtonsoft.Json 走本地 DLL 引用。
- 内嵌 jar：`PCLCS/Launch/JavaWrapper.jar`、`LwjglUnsafeAgent.jar`（启动注入用，移植时原样携带、与语言无关）。

## 2. REA 分析结果（rea-agents 6.8.0，证据在 `rea/` 目录）

对仓库自带的正式版成品 `最新正式版.zip` 内的 exe（6,013,952 字节，SHA256 `0db34e19…c114397`）跑托管静态检查，三项全部成功：

- `inspect-managed-artifact`：PE32/x86、IL-only、.NET Framework CIL、AnyCPU；程序集名 `Plain Craft Launcher 2`、版本 **2.99.0.1**；**23 个程序集引用**（与源码引用清单吻合：WPF、Newtonsoft.Json、NAudio、CacheCow、Imazen.WebP、ThrottleDebounce、MeloongCore(.Wpf)、PCLCS…）。
- `inspect-managed-members`：**665 个类型**的成员/签名清单（`rea/managed-members.json`，261KB）——可作移植时「原版有哪些公开面」的对照表。
- `inspect-managed-native-boundaries`：**26 条 P/Invoke**，集中在 user32（窗口/消息/监视器/按键）、shell32（拖放、管理员判定）、ole32（拖放注销）、advapi32（令牌特权）、ntdll（NtSetSystemInformation）、kernel32（SetDllDirectory）——这就是平台层的确切边界清单，见 §3 第二档。

**REA 在此目标上的适用边界（如实记录）**：

- 通用 `rea analyze` 对该 PE 走原生分析通道，需要 Ghidra/Hopper/IDA/Kuna 之一，本机均未装 → `provider_unavailable`，无结果（`rea/analyze-release-exe.json` 留有原始错误）。
- `rea analyze` 对源码目录被误路由到 JavaScript 应用分析（只挑中 2 个 JSON 文件），对 .NET 源码树无有效产出（`rea/analyze-source-dir.json`）。
- REA 不提供 VB.NET/C# 源码级分析；源码侧结论全部来自常规静态盘点（文件/行数统计、grep、逐文件阅读）。方法级反编译（jeb 类命令）未跑——本项目有完整源码，不需要。

## 3. 许可证要点（`upstream/LICENCE`，原文为准）

PCL 使用自有《PCL 分发有限许可》+《PCL 存储库合理使用指南》，**不是标准开源协议**（GitHub 识别为 NOASSERTION/Other）。指南自述「仅作为说明，不是法律文件」，并承诺善意、合理使用不追究；但其条款对本移植的定性很明确：

- **本移植属于指南定义的「重度使用」**：指南举例包括「参考存储库中较大量的内容，以制作或重新实现 PCL 的部分实质功能（例如启动或下载 Java 版 Minecraft，下载或管理 Mod 等）」——正是本项目要做的事。
- 重度使用的要求（摘录）：① 明确标注是第三方基于 PCL 独立二次创作的产物；② **名称必须以 `Plain Craft Launcher (PCL)` 开头**并加足以表明第三方修改的后缀（示例：PCL XXX修改版；禁止 PCL 3、PCL++、PCL Pro 这类像官方续作的名）；③ 关于页首位给龙腾猫跃署名及赞助链接、公开源代码、继续使用本指南；④ **有 Minecraft 启动功能的，必须实现与 `McLaunchPrecheck` 函数一致（或更激进）的正版购买劝导与赞助劝导**（该函数实测在 `ModLaunch.vb:234`，启动流程 `:111` 调用）；⑤ Windows 上不得有与 PCL 赞助解锁类似的功能或表现，例如界面主色相必须为蓝色。
- 仅参考极小部分代码的「轻度使用」只需署名且不得混淆命名。
- 含义（记录性质，不作法律结论）：移植成果若要公开发布，命名、署名、开源与 `McLaunchPrecheck` 等价实现是许可证写明的硬约束，做计划时已列入 M2/M3 的验收项；仅自用不分发时风险面不同，但本计划默认按「将来可能发布」从严。

## 4. 移植分档

### 第一档：可直接移植的核心逻辑（纯逻辑/数据，逐个配单元测试）

| 原模块 | Rust 落点（crate `pcl-core`） | 备注 |
|---|---|---|
| `PCLCS/McVersion.cs` 版本名语义 | `version.rs`（**已完成样板**） | 含愚人节映射与 1.x 位移规则；发现一处疑似原版笔误（`23w13a` 分支写 `!= "20w13a"`），样板中按原样复刻并注释，是否「修正」待定 |
| Mojang 版本 JSON / 版本清单（manifest）解析 | `version.rs` 模型 + `manifest.rs` | serde 模型已起头；含 inheritsFrom 展开、libraries rules 求值 |
| 启动参数拼装（`ModLaunch.vb` 的参数段） | `launch.rs` | 占位符替换（`${auth_player_name}` 等）、arguments game/jvm 条件项、旧版 minecraftArguments 模板 |
| Java 发现（`ModJava.vb`、`PCLCS/Java.cs`） | `java.rs` | 目录扫描 + `java -version` 探活 + 注册表（仅 Windows 段，见第二档） |
| 下载与校验（`ModNet.vb`、`ModDownload.vb`） | `download.rs` | reqwest + 多源测速/重试 + SHA1/SHA256 校验（sha1/sha2 crate）；任务状态机参考 ModLoader 的 Loader 框架语义 |
| 游戏目录与实例管理（`ModMinecraft.vb`、`McInstallManifest.cs`） | `instance.rs` | .minecraft 布局、versions/ 扫描、McInstallManifest 的 Rust 等价结构（serde） |
| 账号体系数据结构（PageLogin* 背后） | `account.rs` | 离线/微软/authlib-injector/统一通行证四种档案的**数据模型与令牌刷新流程**；OAuth 交互部分属第二档 |
| 崩溃分析规则（`ModCrash.vb`） | `crash.rs` | 以文本规则匹配为主，可移植性好 |
| 整合包格式（`ModModpack.vb`） | `modpack.rs` | zip 清单读写（zip crate） |

### 第二档：需重写的平台层（接口先行，Windows 为主）

- **P/Invoke 26 条**（REA 清单）：窗口管理/前台激活、拖放（shell32/ole32）、管理员判定、令牌特权、监视器信息、全局按键——Rust 侧用 `windows` crate 对应 API 逐条映射；不用的直接砍（如 NtSetSystemInformation 的用途需先在源码确认）。
- **WMI/注册表**：`System.Management` 与注册表读 Java 路径 → `windows` crate / `winreg`。
- **微软登录 OAuth 交互**：原版走内嵌网页/系统浏览器 + 本地回调；Rust 侧改为系统浏览器 + localhost 回调（或 device-code 流程），数据结构沿用第一档。
- **进程与注入**：JavaWrapper.jar / LwjglUnsafeAgent.jar 的挂载方式（classpath/javaagent 参数）原样保留；游戏进程看护（ModWatcher）用 std::process + Job Object（Windows）。
- **音频/图片**：NAudio（主页音乐 ModMusic）→ rodio；Imazen.WebP → image/webp 解码；CacheCow 的 HTTP 缓存 → reqwest 中间件自实现或省略。

### 第三档：WPF UI 的 Rust 替代（整体重写，不翻译 XAML）

PCL 的 UI 是重度自绘（`Controls/` 6,409 行 + 每页 code-behind），且许可证要求 Windows 上主色相为蓝色等约束。四个候选对比：

| 框架 | 形态 | 参考体积* | Windows 贴合度 | 开发量 | 备注 |
|---|---|---|---|---|---|
| **egui** (eframe) | 立即模式、自绘 | 小（单 exe 约 5–15MB） | 中：全自绘，与 PCL 自绘风格天然合拍，但原生控件质感弱 | 低–中：Rust 原生、迭代快；复杂列表/动画需自攒 | 与 PCL「全自绘控件」路线最像，上手最快 |
| **Tauri** | Web 前端 + Rust 后端 | 小（约 3–10MB，用系统 WebView2） | 高：HTML/CSS 还原 PCL 界面最容易 | 中：要维护前端工程（TS/CSS），双语言 | UI 表现力最强；依赖 WebView2（Win10/11 基本自带） |
| **Slint** | 声明式 .slint UI | 小（约 5–10MB） | 中–高：可做原生观感，主题可控 | 中：DSL 学习成本，生态较新 | 声明式与 XAML 心智最接近 |
| **Dioxus** | React 式、WebView 或原生渲染 | 中 | 中：桌面端成熟度低于前三者 | 中–高 | 桌面支持仍在演进，风险相对大 |

\* 体积为同类应用的常见量级估计，非实测。

**建议排序（当时的建议）：egui > Tauri > Slint > Dioxus**。**最终定案（2026-11-14 用户点选）：Slint**——用户先定「不用 WebView」（Tauri / Dioxus 出局）、再点选 Slint（原生渲染、界面现代、动画内置、三大系统支持），详见 §7.5 与 §8.5。

## 5. 已落地的脚手架与样板（本阶段交付物）

- Rust 工具链：rustup 用户级安装，**rustc 1.100.0 / cargo 1.100.0**（`~/.cargo`）。
- Cargo workspace（仓库根 `Cargo.toml`）：
  - `crates/pcl-core`：核心库；`src/version.rs` 为第一个移植模块——Mojang 版本 JSON 的 serde 模型（id/inheritsFrom/type/mainClass/assetIndex/downloads/libraries+rules/arguments/javaVersion）+ PCL `McVersion.NameToVersion`/`Drop` 的忠实移植（含愚人节映射表）。
  - `crates/pcl-cli`：占位 CLI，`pcl-cli <version.json>` 输出 id/类型/库数量/可比版本/Drop 序数。
  - 测试夹具 `crates/pcl-core/tests/fixtures/version-1.20.1.json`：按 Mojang 版本 JSON 结构编写的**结构样本**（个别哈希字段为占位值，仅用于解析测试，不是从 Mojang 抓取的原文件）。
  - 结果：`cargo build`（debug/release）通过；`cargo test` **5/5 通过**；CLI 实跑夹具输出 `1.20.1 / release / 3 libraries / comparable 20.0.1 (drop 200)`，与 PCL 语义一致。

### M1 第一批进展（2026-11-14）

- 新增模块：`pathutil.rs`（MeloongCore PathUtils 的 Windows 字符串路径助手）、`vrange.rs`（JavaVersion＝.NET System.Version 语义、MeloongCore ValueRange、VB Val、PCLCS CompareVersion）、`java.rs`（ModJava.vb＋PCLCS Java.cs：`-version` 输出解析、候选目录、目录搜索与过滤、需求范围计算与选择）、`instance.rs`（ModMinecraft.vb：Newtonsoft 式 JSON 合并、HMCL patches、inheritsFrom 继承链解析、McJsonRuleCheck、McLibGet/McLibListGet、classpath 拼装、实例扫描）、`launch.rs`（ModLaunch.vb＋ModMain.vb ArgumentReplace：参数分割/去重、JVM 与游戏参数组装、占位符替换表与最终组装——纯逻辑，进程拉起留 M2）。
- CLI 子命令已落地：`inspect`（原占位行为）、`name-map`、`scan`、`java list`，均实跑验证。
- 结果：`cargo build` 通过、无 warning；`cargo test` **45/45 通过**（第一阶段 5 个＋本批 40 个）。
- M1 剩余：account 数据模型（Mojang/Microsoft/第三方登录的账号结构）；MeloongCore 实际 API 面盘点（§7.1）；McVersion.GetVanillaName 移植（LiteLoader 的 InheritName 调整依赖它）；ToShortPath 的 Win32 实现（现为注入点）；manifest（version_manifest_v2）展开与下载源——属下载侧，与 M2 download 一并做更合适。（以上除 manifest 与 API 面清单外，均已于第二批完成，见下。）

### M1 第二批进展（2026-11-14）

- 新增模块 `account.rs`（`ModLaunch.vb` 登录数据类＋`Settings.vb` / 登录页用到的账号类设置键）：`McLoginType` 枚举（Legacy=0 / Nide=2 / Auth=3 / Ms=5，忠实保留跳号）；多态登录数据 `McLoginData`（`McLoginMs` 的 OAuthRefreshToken / AccessToken / Uuid / UserName / ProfileJson、`McLoginServer` 的凭据 / BaseUrl / Token / Description / ForceReselectProfile、`McLoginLegacy` 的 UserName / SkinType / SkinName），serde 按各自 JSON 字段名（PascalCase）并按 `Type` 数值分发反序列化；`McLoginResult` 结构；`IsVaild` 校验（含原文文案与 1.20.3 起离线名 16 字符限制）；`GetHashCode` 忠实复刻——MeloongCore `GetStableHashCode` 的 DJB 变体（(r<<5)^r^c、UTF-16、末异或常量）再 Mod Int32.MaxValue，**PCL 的账号相等语义是「哈希相等」**（连具体子类型都不查），连同输入拼接顺序一并复刻；`AuthServerUrl` / `NideServerUrl` 构造器；`AccountStore` 扁平模型取原版散落在 `Settings.vb` 的账号键集（CacheMsV2* ＋过期秒＋V1→V2 迁移标记、CacheNide*、CacheAuth* 与 CacheAuthServer*、`LoginLegacyName` 与 `Login<Nide|Auth>Email/Pass` 的「¨」（U+00A8）连接历史、`LoginMsJson` 有序账号表——条目顺序对齐 Newtonsoft 的 JObject 写入顺序），连带复刻两个原版怪癖：凭据历史重建时长度错位/重名会**整表清零**、Ms 账号表解析失败在页面层会被重置为 `{}`（模型层建模为 `sanitized()`）；`McLoginName()` 的主类型优先＋Ms→Nide→Auth→Legacy 回退顺序。只建模不实现任何真实登录网络流程；测试夹具全为假数据。
- `McVersion.GetVanillaName` 已移植为 `version.rs::get_vanilla_name`（PCLCS `McVersion.cs` ＋构造函数后处理）：15 步判定链（releaseTime/clientVersion/HMCL patches/--fml.mcVersion/inheritsFrom/downloads 正则/libraries 四模式/jar/version.json/VersionPattern/文件夹名/全文，含 reliable 标记语义与构造函数的 null→Unknown、`_unobfuscated` 后缀剥除）；Rust `regex` 不支持 lookbehind，三处 `(?<=...)` 已改写为非捕获前缀＋捕获组；原版两处空引用抛错点（无 libraries / 无 id 时）按「被上层 catch 后回退 Unknown」建模。发现一处原版可复现怪癖：**`24w14potato` 匹配不上 VersionPattern**（w 后需 [a-g] 结尾或点分数字），裸 JSON 最终落到 Unknown/unreliable——已按此写断言。`instance.rs` 的 LiteLoader `InheritName` 调整随之解锁：新增 `inherit_name_of`，`ResolveOutcome` 增加 `inherit_name` 字段，继承链推进改用父级的调整后名字（与 VB getter 语义一致，深度 ≥3 的库重复合并怪癖不变）；jar 内 version.json 在解析器层取不到，检测的最后兜底步在此上下文缺位（已注释，仅在其他步骤全落空时有影响）。
- `PathUtils.ToShortPath` 已落地（`pathutil.rs`）：字符串层算法完整移植（快速返回、保留已存在文件名、向最近的已存在祖先逐级上溯、pathToKeep ≥200 放弃），文件/目录存在性与短名获取为注入点以便跨平台测试；连带移植 `ToExtendedFormat` / `ForApi`。平台短名获取：`cfg(windows)` 下 `win32` 子模块是全仓唯一 unsafe 边界——单个 `GetShortPathNameW` FFI、220 字符缓冲、返回 0 或超长回退原路径（与原版 `TryGetShortPath` 一致）；非 Windows 恒等。**一处刻意不移植并已在代码注释标明**：原版在合并后仍超长时会真实**生成** 8.3 名（CreateFileW＋SetFileShortNameW＋RtlGenerate8dot3Name，会改用户文件系统），移植版在该罕见分支返回合并后的长路径——是否启用留待产品决策。验证状态：Linux `cargo build` 无 warning、`cargo test` **66/66 通过**（45 基线＋本批 21：account 11、version 4、instance 2、pathutil 4）；Windows 分支经 `cargo check --target x86_64-pc-windows-msvc` **真编译（类型检查）通过**，但未在 Windows 上实际运行。
- M1 剩余：manifest（version_manifest_v2）展开与下载源（随 M2）；MeloongCore 实际 API 面全量清单未单独成文（第一、二批已顺带覆盖 PathUtils / StringUtils / ValueRange 中主程序实际用到的部分）。

### M2 进展（2026-11-15，CLI 真能启动游戏）

- **HTTP 层** `http.rs`（`ModNet.vb`）：`HttpClient` trait（业务代码只依赖 trait，单测注入假响应）；`ReqwestClient` 复刻 `NetRequestByClientRetry` 的重试语义——最多 3 次、超时 10s/30s/后备请求 4s、403/404 不重试、429 睡 10 秒、主 URL 失败切换 backup URL。代理走标准环境变量（PCL 的 .NET 客户端走系统代理的等价行为）。TLS 后端按目标分流：Windows 用 native-tls（系统 TLS，与上游一致）；其他平台用 rustls＋系统证书库——reqwest 的 native-tls 连接器不发 ALPN，本 VM 的出口代理会黑洞无 ALPN 的 ClientHello（实测定位：curl/python 带 ALPN 正常、无 ALPN 握手超时），且 rustls 默认 Mozilla 根不认代理拦截 CA，故选 native roots。
- **下载源** `download.rs`（`ModDownload.vb` 的 `DlSource*` 全套）：官方↔BMCLAPI 改写按原版逐条复刻——Mojang 域名前缀替换（launchermeta/piston-meta→清单、piston-data→版本 JSON、libraries/resources 的 metadata 路径保留、forge/fabric maven 路由、OptiFine 的 LocalPath 重组、未知 Mojang 域名记日志且不改写）、`ListSource.TryGetValue` 的 key 归一化（去版本号尾段/forge 文件名剥离/`_pre`→preview_）、官方需求的 `IsOfficial` 二分（1KB/s 测速逻辑建模为注入接口，CLI 默认 `official_fast` 即 DlPreferMojang 测定后的常态值）；`BMCLAPI_ROOT` 等端点常量与原版一致。同时落地 `FileChecker`（`ModBase.vb`：大小容差 max(实际−1KB, 实际*0.995)、哈希按长度分发 MD5/SHA1/SHA256、<1KB 错误页启发式）与下载引擎：线程分片（<1MB 整文件、其余 32 线程分片，Range＋Content-Length 双校验）、两轮重试（多源并发轮→单线程轮，复刻 `DlFileDownloader` 结构）、`download_all` 并发调度＋失败聚合。真实 HTTP 集成测试走本地 TcpListener 服务器（整体/分片/失败转移/校验失败四场景）。
- **版本清单** `manifest.rs`：version_manifest_v2 拉取（清单源顺序与原版一致：piston-meta v2→launchermeta v1→BMCLAPI 镜像）、磁盘缓存回退、≥200 条 sanity 校验、latest release/snapshot 与按 id 定位、`normalize_version_id`（原版清单 id 归一化）、版本 JSON 按 sha1 路径缓存下载。
- **客户端补齐** `mcfix.rs`（`DlClientFix` 等价）：client jar（`DlClientJarGet`）+ 继承链 libraries（`mc_lib_list_get_for_os`）+ asset index + assets 对象的两阶段下载计划（阶段 1 并发下完才进阶段 2，与原版 Loader 组合一致）；assets 对象 URL 只走官方/BMCLAPI 改写（原版同）。路径互转在 PCL 的 Windows 字符串形态与宿主原生路径之间做（§8 抽象点）。未移植：nide8auth / authlib-injector 的附加下载（第三方登录服务器的 jar 分发，待登录真机验证时一并做）、Forge/OptiFine 等安装器流程（M2 验收外）。
- **实例 OS 参数化** `instance.rs`：M1 的 `rule_check` / `lib_list_from_json` 保持 Windows 签名不变，新增 `rule_check_for_os` / `lib_list_from_json_os`（os_key ∈ windows/linux/osx）——规则求值与 natives classifier 按宿主 OS 解析，这是上游只有 Windows 时不存在的跨平台适配点，其余语义（合并、去重、怪癖）不动。
- **登录链路** `login.rs`（`ModLaunch.vb` 登录段＋`MyMsgLogin.xaml.vb`＋`ModSecret.vb`）：MS 全链路状态机——缓存命中（CacheMsV2Expires 未过期＋名字匹配；ClientToken 固定取 UUID 是原版怪癖）→设备码流（consumers 端点、2 秒轮询、authorization_pending 续轮、错误串映射含 AADSTS70000→Restart）或 login.live.com 刷新（Relogin 标记串集）→XBL（RpsTicket 需 `d=` 前缀）→XSTS（XErr 2148916227/33/35/38 原版文案）→`login_with_xbox`（过期＝now+expires_in−3600）→entitlements 空判未购买→档案获取（404＝未建档案）。OAuth client id 读环境变量 `PCL_MS_CLIENT_ID`（上游 `ModSecret.vb` 开源版该常量为空）。Legacy 离线 UUID 算法（原版 M1 模型未含，本批补齐：hex(UTF-16 长度)＋hex(稳定哈希）各补足 16 位、第 12 位改 3、第 16 位改 9；皮肤类型 7/15/23/31 位异或奇偶；类型 1/2/4 尾 5 位递增试探；类型 3 经注入闭包查 Mojang UUID 且需版本 <20）。yggdrasil（Auth/Nide 共用）：缓存门→validate（带 requestUser）→refresh→authenticate（请求不带 clientToken、多档案无选中档案时置 NeedRefresh 再 refresh 一轮）。**全部以假响应夹具单测，未用任何真实凭据，MS 真实登录未真机验。**
- **启动前检查** `precheck.rs`（`McLaunchPrecheck` 等价，许可证验收项）：游戏路径含 `!`/`;` 报错→实例自身 Error 报错→`McLoginAble` 校验→赞助提示（仅 Release 模式、启动计数 ∈ {10,20,40,…,2000} 时、链接 meloong.com/afd/a/LTCat）→正版提示（已 HintBuy 或 Ms 登录跳过；中文环境非阻塞、计数 ∈ {3,8,15,…,1900}；非中文＋Legacy 为阻塞门，购买/试玩（--demo)/取消三选建模为返回值）；SaveBatch（批量保存）时两提示全跳过。提示一律建模为数据返回、由 CLI 呈现（核心层不做 UI)。
- **进程拉起** `runner.rs`（`McLaunchNatives` / `McLaunchRun` 等价）：natives 按宿主扩展名（dll/so/dylib）解压、同大小跳过、残留文件删除、损坏 jar 删后报错；进程以 java（非 javaw）直启、工作目录＝PathIndie、Path 环境变量**追加** Java 目录（去重）、appdata 指向 MC 文件夹、stdout/stderr 管道实时透传。未移植：`McLaunchMemoryOptimize`、进程优先级设置、低速看门狗（256KiB/s）与下载限速、authlib prefetch。
- **CLI**（`pcl-cli/src/m2.rs`）：新子命令 `versions`（清单＋来源＋latest)、`install <id>`（版本 JSON→父链→两阶段补齐）、`launch <实例>`（precheck→登录 legacy/ms/auth/nide→补下载→natives→Java 需求判定与宿主发现→参数拼装→spawn 透传；`--dry-run` 只打印命令；`--isolate` 对应版本隔离设置）。账号持久化为 CLI 层 JSON 文件（环境变量 `RERUST_PCL_STORE`，默认 `~/.rerust-pcl/store.json`)，启动计数/HintBuy 存同目录 launch-state.json；服务器登录密码只从环境变量读，不落盘、不入参数。
- **验证证据**：`cargo build` 0 error 0 warning；`cargo test` **95/95 全绿**（M1 的 66＋M2 新增 29：manifest 4、precheck 5、login 8、download 5、mcfix 4、runner 2、instance OS 变体 1)；`cargo check --target x86_64-pc-windows-msvc` 通过（TLS 分流后 Windows 走 native-tls、无 C 工具链依赖）。实网验证：`pcl-cli versions` 走官方源成功（923 个版本、latest release 26.3 / snapshot 26.4-snapshot-8，约 1 秒）；`pcl-cli install 1.5.2` 实网全量安装成功（exit 0：版本 JSON＋阶段 1 的 11 个文件（client jar/13 库中适用部分/资源索引）＋阶段 2 的 749 个资源对象（pre-1.6 虚拟资源），共 57MB，全部 SHA1 校验通过）。全链路集成验证（本地 HTTP 服务器＋假 Java 21):`launch` 走完 Legacy 登录→4 文件补下载（含 SHA1 校验；23 字节假 jar 被 FileChecker 的 1KB 下限规则正确拦截、垫齐后通过）→assets 预置命中免下载→natives 解压→参数拼装→真 spawn 透传输出、退出码 0;`--isolate` 时 `--gameDir` 正确指向版本文件夹。**未验**：真实 Java 启动真实游戏（VM 无 Java、无正版账号）、MS/yggdrasil 真实登录、Windows/macOS 真机运行（Windows 仅类型检查、macOS 待 runner）。

### M2 真机验证（2026-11-15，Windows PC 真机）

用户 2026-11-15 08:35 点选「先把 CLI 弄到我 PC 上真机验一遍」，启动目标版本为 **1.0（用户指定）**。验证分两条线：(A) 现成 .minecraft 的检测与启动；(B) rerust-pcl 自有目录的全新 install。

**构建路线（可复现）**：VM 无 MSVC 工具链，走 cargo-xwin——`cargo install cargo-xwin`（0.23.1）后 `cargo xwin build --release --target x86_64-pc-windows-msvc -p pcl-cli`（xwin 自动下载 MSVC CRT/Windows SDK 并用 rust-lld 链接）。产物 `target/x86_64-pc-windows-msvc/release/pcl-cli.exe`，PE32+ x86-64 控制台程序，4,923,904 字节；经 SFTP 部署到 PC `C:\Users\luke\Desktop\files\rerust-pcl\pcl-cli.exe`。Windows 目标的 native-tls（schannel）在此路线下正常链接。

**通用项（PC 真机）**：

- `--help` 正常输出用法行，退出码 0。
- `java list` 找到 3 个 Java：Oracle JDK 21（21.0.8.0，`C:\Program Files\Java\jdk-21`）、Temurin 25（25.0.5.0，全局默认）、HMCL 托管的 Mojang legacy JRE 8（8.0.51.0，`%APPDATA%\.hmcl\java\windows-x86_64\mojang-jre-legacy`）。
- `versions` 走 PC 直连网络拉官方清单成功：来源 Mojang 官方源、总数 923、latest release 26.3 / snapshot 26.4-snapshot-8，与 VM 结果一致。

**(A) 现成 .minecraft 检测/启动线**：现成游戏目录不在 HMCL 注册的 `%APPDATA%\.minecraft`（该处只有 cache），实际在 `C:\Users\luke\Desktop\files\game\.minecraft`（与 HMCL-3.16.4.exe、PCL 同目录），versions 下有 4 个实例：`1.0`、`Fabulously Optimized`、`Fabulously Optimized 26.3`、`Fabulously Optimized 26.3 2GB`；其中 1.0 是版本隔离实例（夹内含 saves/mods/options.txt 等）。

- `scan` 对 4 个实例全部检出且识别正确：`1.0` → id=1.0、mainClass=net.minecraft.launchwrapper.Launch、库 9（原始 JSON 12 条经 Windows 规则过滤后的适用数，`inspect` 读原始 JSON 为 12，两者口径不同、均正确）；3 个 FO 实例均识别为 Fabric KnotClient 主类、库 78/82/82。实例名、版本 id、主类与目录实际内容逐项对得上。
- `launch 1.0 --dry-run`（Legacy 离线假账号 RerustTest、`--isolate`、`--java-dir` 指向 HMCL 的 JRE 8）：登录完成（UUID 000000000000300A9D84218A04C3D4AB）、DlClientFix 两阶段校验通过、natives 解压完成、选定 Java 8.0.51.0、参数拼装完整打印（工作目录正确指向 versions\1.0）。
- **真实启动：进程成功 spawn（PID 3424），但游戏未起来，退出码 1**——JVM 报错「找不到或无法加载主类 net.minecraft.launchwrapper.Launch」。**这是真 bug、不是会话 0/无显示的环境问题**：dry-run 命令行里所有 classpath 条目都是盘符相对路径 `C:Users\luke\...`（盘符后丢了根 `\`），JVM 按工作目录相对解析自然找不到任何库。根因已定位：`pcl-core/src/mcfix.rs::to_native_path` 把 PCL 形态路径按分隔符拆分后逐段 push 进空 PathBuf，根组件被丢弃，`C:\...` 被重组为 `C:...`；同命令里经 `native()` 直出的路径（natives 目录、--gameDir）均正确，只有走 PCL 形态→native 转换的 classpath 中招。修复方向：`to_native_path` 对 Windows 盘符路径保留根（或整体走 PathBuf 语义），并补一条在 Windows 上真跑的往返断言（现有断言未在 Windows 真机跑过）。
- 附带发现一：launch 的 Java 自动发现只探到 Java 25，未命中 HMCL 托管的 JRE 8（`java list` 能找到），老版本启动需 `--java-dir` 显式指定；自动发现的候选目录清单待补 HMCL 的 java 存放路径。
- 附带发现二：CLI 的语言环境判定读 LC_ALL/LANG 环境变量，Windows cmd 下通常为空 → 被判为非中文环境，Legacy 离线启动会撞上阻塞式正版三选门（需 --demo-gate）。本次靠给 CLI 进程设 `LANG=zh_CN.UTF-8` 绕过；原版 PCL 读的是 Windows 系统区域设置，移植版应改读系统 locale，否则中文 Windows 用户会被误判。
- 对现成实例数据的影响：首次 dry-run 时 DlClientFix 按正常启动器行为向该 .minecraft 补下 9 个库文件、解压 9 个 natives 文件（后续运行均 0 补下）；游戏本体未运行起来，存档等用户数据未被触碰。

**(B) 全新 install 线（自有目录）**：`install 1.5.2 --mc C:\Users\luke\Desktop\files\rerust-pcl\.minecraft` 全量成功，退出码 0：版本 JSON 1 个＋阶段 1 共 11 个文件（含 1.5.2.jar 5,564,661 B）＋阶段 2 共 749 个资源对象，SHA1 校验全过，落盘总大小 55,153,030 字节，与 VM 实网结果（11 文件＋749 对象）一致。

**未验项**：游戏真正跑起来（被上述 classpath bug 挡住，修好后需重验，且会话 0 无显示环境下 LWJGL 能否建窗仍是未知数）；MS/yggdrasil 真实登录；macOS 真机。

### M2 真机验证后续：三修与 1.0 真启动重验（2026-11-15）

用户点选「先修 classpath bug 并在 PC 重验 1.0 真启动」，本轮修三处并完成真启动重验：

- **classpath 根丢失（主修）**：`mcfix.rs::to_native_path` 改为字符串层转换——先剥出根前缀（盘符＋根分隔符、UNC 的 `\\server\share`、无盘符根、盘符相对 `C:foo` 原样保留），其余组件按宿主 `MAIN_SEPARATOR` 重组，不再逐段 `PathBuf::push`（push `C:` 后再 push 组件得到的是盘符相对路径，这正是根丢失的机制）。`mcfix` 的 `path_round_trip` 测试重写（旧断言本身把错误结果 `C:MC\...` 写成了期望值）：新增盘符根、盘符绝对、正斜杠输入、盘符相对、UNC 根与子路径、无盘符根、相对路径与精确往返共 9 组断言。
- **语言判定（必修）**：新增平台事实 `platform::user_locale_name()`——Windows 下经既有 Win32 边界（`pathutil::win32`，新增 `GetUserDefaultLocaleName` FFI 与 cfg 分发的 `pathutil::user_default_locale_name()`）读 OS 用户区域名（如 zh-CN）；CLI 侧 `detect_chinese_locale()` 改为 Windows 优先用该来源、失败才回退 LC_ALL/LANG，非 Windows 平台保持原 LC_ALL/LANG 逻辑。重验全程未设 LANG。
- **Java 自动发现（可选，已做）**：根因有两层——候选目录 PCL 字符串此前同样被 `to_native_path` 压成相对路径（只有绕过它的 JAVA_HOME 能命中，所以上轮只探到 Java 25）；且 HMCL 的 JRE 嵌在候选根 `.hmcl\java\<平台>\<运行时>\bin` 三层之下、`Program Files\Java\<jdk>\bin` 也隔一层，根部直探够不着。`m2.rs::discover_javas` 改为对候选根做深度 3 的递归收集（`collect_java_bins`，与 `java list` 的递归 `search_folders` 语义对齐），新增临时目录树单测（HMCL/JDK/直放三种布局＋深度限制）。
- **构建与部署**：`cargo test` **96/96 全绿**（core 95＋cli 新增 1）、`cargo build` 0 warning、Windows target check 通过；`cargo xwin build --release` 产物 4,929,536 字节，SFTP 覆盖 PC 的 `rerust-pcl\pcl-cli.exe`，旧 exe 备份为 `pcl-cli.exe.bak_2026-11-15`。
- **dry-run 重验**（证据 `dryrun-10b.txt`，无 LANG、无 --java-dir）：classpath 8 条全部为带根绝对路径 `C:\Users\luke\...\libraries\...;...versions\1.0\1.0.jar`；自动发现直接选中 HMCL JRE 8（8.0.51.0）；precheck 未撞正版门、EXIT=0。
- **真实启动重验（通过）**（证据 `launch-10b.log`）：进程 spawn 成功（PID 12040，会话 0），游戏日志正常初始化——LaunchWrapper 加载（`Using tweak class name net.minecraft.launchwrapper.VanillaTweaker`、`Launching wrapped minecraft`）、`27 achievements / 174 recipes`、`Setting gameDir to: ...\versions\1.0`、jinput 控制器环境加载、声音系统启动且 `OpenAL initialized.`；日志稳定在 52 行、进程存活停在标题画面阶段。非致命噪声两处（均为 1.0 原版年代行为、与本次修复无关）：virtual/legacy 图标文件缺失致窗口图标读取失败、旧 `s3.amazonaws.com/MinecraftResources` 资源检查 URL 已失效（该线程报错退出、不影响主流程）。日志取证后以 `taskkill /F` 只结束本轮拉起的游戏进程（命令行已逐项核对为本次启动），CLI 随之记录的 exit code 1 是强制终止的产物、不是游戏崩溃；PC 上无本轮残留进程，用户会话 1 的 java 与 mcserver 均未碰，存档未触碰（--isolate 工作目录照旧）。注：`start /min` 脱离 SSH 会话拉起在本机不驻留（bat 未执行、无日志生成），真启动改走 SSH 前台通道完成。
- **仍未验**：游戏窗口在用户桌面会话里的实际可见画面与标题画面交互（会话 0 无法显示，需用户在桌面会话亲自跑一次才能确认）；MS/yggdrasil 真实登录；macOS 真机。

## 6. 里程碑

- **M1 核心库可测**：`pcl-core` 完成 version/manifest（含 inheritsFrom 展开与 rules 求值）、java 发现、instance 扫描、account 数据模型；每个模块带从原版行为对照的单元测试；CLI 提供 `scan`/`java list` 子命令。此阶段不碰 UI、不真启动游戏。
- **M2 CLI 可启动游戏**：download（多源+校验）、launch（参数拼装+进程拉起+日志看护）、微软登录（浏览器 OAuth）打通；**验收含许可证硬约束**：`McLaunchPrecheck` 等价的正版/赞助劝导已实现；能在真实 .minecraft 上从零下载并启动一个原版版本（先原版，后 Fabric/Forge）。
- **M3 GUI**：UI 框架按用户拍板结果接入（默认按建议排序推进）；页面按 PCL 信息架构复刻（启动/下载/实例/设置），关于页署名与命名按许可证要求（名称以 Plain Craft Launcher (PCL) 开头 + 第三方后缀、蓝色主色相）。

## 7. 风险与待定

1. **MeloongCore 依赖**：主程序大量调用该库（5,812 行），盘点需在 M1 前补一轮「主程序实际用到的 MeloongCore API 面」清单，避免移植时才发现缺口。
2. **原版笔误的取舍**：`McVersion.cs` 中 `23w13a` 分支的 `!= "20w13a"` 疑似笔误——「忠实复刻」还是「按意图修正」，M1 时逐个列单再定，样板已按复刻处理并注释。
3. **.NET Framework 4.8 特性**：旧式 vbproj + 本地 DLL 依赖意味着无法用现成工具做 VB→C# 机械转换再对照，只能人工读 VB.NET 移植（VB 语法与 C# 差异不大，阅读成本可控）。
4. **许可证命名约束**：成果名不能自创（如不能叫 PCL-Rust），M3 发布前按 §3 复核。
5. UI 框架**已定案**（2026-11-14 用户点选）：**Slint**——原生 GPU 渲染、不用 WebView（Tauri / Dioxus 因此出局），三大系统均支持；Slint 自有授权（桌面 royalty-free）与 PCL 分发许可的兼容在公开发布前须复核（见 §8）。

## 8. 跨平台（Windows / macOS / Linux）设计约束（2026-11-14 用户定）

ReRust_PCL 必须同时支持 Windows、macOS、Linux 三大系统，以下约束对全部后续批次生效：

1. **核心逻辑平台中立**：`pcl-core` 不写死任何单一平台假设——不硬编码盘符/注册表/Win32 路径形态作为逻辑前提；上游 PCL 的 Windows 专属行为（短路径、注册表设置、计划任务等）一律落到平台层，核心层只留模型与纯函数。账号模型即按此执行：`AccountStore` 是键值形状的中立模型，其持久化后端（原版为 Windows 注册表加密项）在三平台分别落为注册表 / 文件 / 钥匙串（具体选型 M2/M3 定），模型本身不变。
2. **平台差异走 cfg / 抽象层**：已落地的形态——短路径获取 `#[cfg(windows)]` 走 `GetShortPathNameW`、其他平台恒等回退，算法层经注入点与平台无关；Java 发现的候选目录/可执行名差异（`java.exe` vs `java`）、实例路径分隔符等同理，新增平台行为先找抽象点再写实现，禁止在核心算法里散落 `cfg`。
3. **unsafe 收敛**：平台 FFI 只能出现在独立平台子模块内（如 `pathutil::win32`），单个函数一个边界、失败回退原行为，核心其余部分保持 safe。
4. **验证口径**：VM（Linux）上 `cargo build` / `cargo test` 必须全绿；Windows 分支用 `cargo check --target x86_64-pc-windows-msvc` 做类型检查验证（已配好 target），运行验证需真实 Windows 机器；macOS 的编译与签名在 Linux VM 上做不了，后续需 Mac 或 CI 的 macOS runner（待安排，在此之前 macOS 分支只保证源码层 cfg 结构与可注入点齐全，不宣称已验证）。
5. **UI 层**：M3 用 Slint（原生渲染、不用 WebView、三大系统支持）；其授权与 PCL 分发许可的兼容在公开发布前复核。

### M3 第一批进展（2026-11-15，Slint GUI 骨架）

详见 `M3-batch1-REPORT.md`。要点：新建 `crates/pcl-ui`（Slint 1.18，无 WebView）——主窗口按原版 FormMain 组织（48px 蓝色标题栏＋一级导航 启动/下载/联机/设置/更多＋启动页左 300px 启动列/右实例列表与日志），窗口标题与显示名按用户定为「PCL ReRust」，工程/仓库名 PCL_ReRust；实例扫描已接 `scan_instances`，启动按钮已接通 precheck→离线登录→继承/库/natives→Java 需求与发现→参数拼装→`runner::spawn_game` 全链（Spawner 抽象＋端到端单测）；下载/联机/设置/更多为占位，关于区已预埋龙腾猫跃首位署名。验证：`cargo test --workspace` 100/100；Linux release 真二进制（pcl-ui 33,808,032 B）；Windows cargo-xwin release 真 exe（pcl-ui.exe 16,526,848 B）；macOS `cargo check --target x86_64-apple-darwin` 卡在依赖 ring 的 C 构建脚本（需 Apple 目标 C 工具链/macOS SDK，本 VM 无 clang/osxcross），属环境卡点，真实构建待 macos runner 或 Mac 上 `build-macos.sh`。代码已推 GitHub 公开仓 `lukeqiang2000/PCL_ReRust`；Actions 工作流因 gh OAuth 令牌缺 `workflow` 权限范围未能推送（文件留本地，upstream 自带 workflow 同因未纳入），待用户 `gh auth refresh -h github.com -s workflow` 后补推。
