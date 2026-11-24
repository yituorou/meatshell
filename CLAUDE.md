# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概览

MeatShell 是 Rust + Slint 实现的 FinalShell 风格 SSH/终端客户端，单二进制承载三种入口：
桌面 GUI、CLI 自动化、MCP 服务。MSRV 为 Rust 1.88（见 `Cargo.toml` 的 `rust-version`）。

## 常用命令

```bash
# 桌面开发运行；改 .slint 后 cargo check 是最快的反馈方式
cargo run --release
cargo check

# 无 GUI 构建（服务器/CI）：排除全部 Slint 桌面代码，产物只保留 CLI/MCP 与 SSH/SFTP
cargo build --locked --features headless

# 单元测试（CI 同款）。--bin meatshell 限定只跑 src 内单元测试，
# 不编译 tests/ 下依赖 Slint 的集成测试（headless 下 build.rs 不生成 Slint 代码）
cargo test --locked --features headless --bin meatshell
# 单测过滤示例
cargo test --locked --features headless --bin meatshell ssh::cwd::tests

# 桌面侧校验：headless 不编译桌面控制器，必须单独 check 一次 Rust/Slint 集成
cargo check --locked --bin meatshell
# tests/ui_*.rs 是默认 feature 下的 Slint 集成测试（MinimalSoftwareWindow 无头渲染，
# 无需显示器；部分测试支持 MEATSHELL_*_QA_DIR 环境变量导出 PNG 帧）
cargo test --locked --test ui_recents

# Python E2E（loopback 合成服务器，不连真实主机、不用真实凭据）：
# 先构建 headless 产物，再执行脚本（需 pip install paramiko）
cargo build --locked --features headless
python tests/ssh_jump_chain_e2e.py --exe target/debug/meatshell
python tests/config_import_e2e.py --exe target/debug/meatshell
python tests/remote_mcp_e2e.py --exe target/debug/meatshell   # CI 中解释器为 python3

# 依赖安全审计（配置见 audit.toml）
cargo audit
```

测试组织与完整回归矩阵见 `tests/CONNECTION_REGRESSIONS.md`。`tests/app/` 下的测试通过
`#[path = "..."]` 作为 `app` 模块的子模块编译，以访问私有 helper，不是独立 integration target。

**发版只能走脚本**，禁止手改 `Cargo.toml` 版本号后打 tag：

```powershell
./scripts/release.ps1 v0.8.4 -Push
```

脚本会同步 `Cargo.toml`/`Cargo.lock`、跑 `cargo check --locked`、校验 `--version`、提交并建
annotated tag。Release workflow 强制校验 tag 名、Cargo.toml、Cargo.lock、二进制 `--version`
四者版本一致，详见 `docs/release.md`。

## 架构

### 三入口与 headless 裁剪

`src/main.rs` 按 `StartMode` 分派：无子命令 → Slint GUI；`cli` 子命令 → `src/cli`；
`mcp serve` → `src/mcp`（stdio 与可选的 Streamable HTTP，后者必须显式指定 `--data-dir` /
`MEATSHELL_DATA_DIR`，见 `docs/REMOTE_MCP.md`）。

CLI 与 MCP 只是 `src/automation` 的两个适配层（协议封装、参数呈现、输出格式各自负责），
共享同一份会话配置与 SSH/SFTP 实现，不存在第二套服务器配置。

`headless` feature 在两处裁剪桌面代码：

- `main.rs` 中 `app`/`ui`/`session`/`rdp`/`resource`/`layout`/`update`/`wallpaper`/`webdav`
  等模块整体 `cfg` 排除，`terminal` 切换到 `src/terminal/headless.rs`；
- `build.rs` 检测到 `CARGO_FEATURE_HEADLESS` 后提前返回，不调用 slint-build。

因此改桌面模块后，headless 测试通过不代表桌面侧编译通过，必须补一次
`cargo check --locked --bin meatshell`。

### UI 层（Slint）

- 界面声明在 `ui/*.slint`，由 `build.rs` 以 fluent style 编译并打包 gettext 翻译；
  `src/ui` 仅封装 Slint 生成的组件类型。
- `src/app.rs` 是顶层 UI 状态机（加载配置、1Hz 系统采样、标签页与 SessionHandle 管理、
  回调路由），实际实现拆在 `src/app/` 下 50 余个按功能命名的子模块（sftp_*、session_*、
  tab_*、terminal_ui 等），改 UI 行为先在其中定位对应子模块。
- Slint 事件循环单线程，所有后台线程（SSH worker、采样、下载）的 UI 更新必须经
  `slint::invoke_from_event_loop` 回到 UI 线程。
- 多窗口为 Chrome 式单进程管理（`src/app/single_instance.rs`），关窗进托盘后会话保活。

### 模块布局约定

领域模块（`ssh`、`sftp`、`config`、`session`、`terminal`、`cli`、`mcp`、`automation`）
统一采用 `mod.rs` + `struct/`（数据结构）+ `impls/`（方法与功能实现）布局，`mod.rs`
通过 `#[path = ...]` 挂载子文件并 re-export。新增代码沿用该拆分，不要把大模块摊平成单文件。
模块内单测就近放在 `impls/*_tests.rs`。

### SSH / 终端 / SFTP

- SSH 用 `russh`（ring 加密后端，含 legacy 3DES 与 1024 位 RSA 主机密钥兼容），SFTP 用
  `russh-sftp`；SSH/SFTP/跳板共享 `known_hosts` 校验（`src/ssh/impls/known_hosts.rs`），
  首次连接确认、密钥变更再次提示。
- 出站 SOCKS5/HTTP 代理在 `src/ssh/impls/proxy.rs`；跳板链路由 `src/config/jump_chain.rs`
  与 ssh 连接层共同处理；PuTTY PPK v2/v3 加载在 `impls/ppk.rs`（全程内存解密）。
- 终端模拟基于 `vt100` crate（版本钉死，升级前先读 `Cargo.toml` 中对应 issue 注释），
  本地 shell/串口/Telnet 走 `portable-pty` 与 `serialport`；彩色 emoji 由内置 Twemoji PNG
  替换渲染，终端字体缺字时按 build.rs 预生成的字形覆盖表回退系统字体。

### 配置、凭据与数据目录

- 会话库为平台配置目录下的 `sessions.json`（Windows `%APPDATA%/meatshell/` 等），
  可用 `--data-dir` 或 `MEATSHELL_DATA_DIR` 重定向；多进程写由 `fs2` 文件锁串行化。
- 会话密码用 ChaCha20-Poly1305 加密落盘，密码缓冲 `zeroize` 清零；便携导出用
  `enc:exp:v1:` 前缀的内置密钥方案，按敏感文件对待。
- CLI/MCP 返回结果不得包含明文凭据；MCP 还有独立的功能授权开关与「允许 MCP 访问此会话」
  粒度控制。

### vendor 中的 Slint 补丁

`Cargo.toml` 的 `[patch.crates-io]` 用 `vendor/` 下三个本地 crate 覆盖官方
`i-slint-backend-winit`、`i-slint-core`、`i-slint-compiler`（原生编辑器语法着色、行号槽、
Wayland/macOS 窗口修复，细节见 `vendor/EDITOR_PATCH.md`）。三者必须同时升级、不可只移除
其中一个 override；Slint 全家桶版本钉在 `=1.18.1`，升级时连 `slint-build`、
`i-slint-backend-winit` 一起动。补丁点均带 `MeatShell editor patch` 注释。

### 国际化

源语言是中文——Slint `@tr(...)` 的 msgid 即原始中文字符串。翻译文件为
`lang/{zh,en}/LC_MESSAGES/meatshell.po`，由 build.rs 打包进二进制，运行时通过
`slint::select_bundled_translation` 切换。新增/修改界面文案需同步更新 `.po` 文件。

### 平台条件编译

平台差异密集且均有 issue 注释，改动前先读 `Cargo.toml` 对应段落：Windows 用 mimalloc、
Unix 用 jemalloc（`src/allocator`）；tray-icon 不用于 macOS（Objective-C 类冲突）；Skia
渲染器仅在 macOS 编译（默认仍走软件渲染，femtovg/Skia 可在设置切换）；Windows 的 SFTP
拖出与跳转列表走 COM（`windows` crate），macOS 拖出走 objc2 file promises。

## 注意事项

- release profile 为 `panic = "abort"` + thin LTO + strip，依赖 `catch_unwind` 的逻辑不能
  依赖 release 行为；测试走 dev profile 不受影响。
- `Cargo.toml` 中大量依赖被精确钉版本并附 issue 号说明原因（vt100、russh、unicode-width、
  ssh-key RC 版等），升级任何钉版依赖前先读其上方注释与 `audit.toml`。
- `Cargo.lock` 虽在 `.gitignore` 中，但 release 流程依赖它校验版本，发版脚本会维护，
  不要手动删改。
- 日志统一走 `tracing`；`icu_provider` target 已在 logging 初始化中静默，不要用
  `eprintln!` 兜底新增第三方库噪音。
