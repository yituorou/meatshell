# meatshell

**简体中文** | [English](./README.en.md)

一个轻量级、低内存占用的 SSH / 终端客户端，灵感来自 FinalShell，但完全由
**Rust + [Slint](https://slint.dev)** 实现。目标是保留 FinalShell 的核心体验
（资源监控侧栏、会话管理、多标签页终端）的同时，把内存占用从 400 MB+ 的
JVM 压到几十 MB 原生级别。

## 截图

<p align="center">
  <img src="docs/screenshots/01-welcome.png" alt="欢迎页 / 会话管理" width="800"><br>
  <em>欢迎页：会话管理 + 左侧本机资源监控</em>
</p>

<p align="center">
  <img src="docs/screenshots/02-terminal-htop.png" alt="终端 + SFTP" width="800"><br>
  <em>多标签页终端（htop 全屏渲染）+ 底部 SFTP 文件浏览 + 远端资源监控</em>
</p>

## 下载与安装

每次打 `v*` 标签，GitHub Actions 会自动构建 **Windows / Linux / macOS** 三平台二进制，
发布到 [Releases](https://github.com/jeff141/meatshell/releases) 页面。

### Windows

下载 `meatshell-*-windows-x86_64.zip`，解压后双击 `meatshell.exe`。

### Linux

```bash
tar -xzf meatshell-*-linux-x86_64.tar.gz
cd meatshell-*-linux-x86_64
./meatshell                                  # 直接运行
# 可选：装应用图标 + 启动器入口（Dock / 应用列表里显示图标，无需传参）
chmod +x install-linux.sh && ./install-linux.sh
```

> 需要 glibc ≥ 2.35（Ubuntu 22.04+ / Debian 12+）。Wayland 下首次装完图标可能要注销重登一次。

从源码 `cargo run`（Linux Mint / Ubuntu / Debian）需要先安装 Slint/winit/rfd 等用到的系统开发包：

```bash
sudo apt update
sudo apt install -y --no-install-recommends \
  build-essential pkg-config cmake \
  libfontconfig1-dev libfreetype6-dev \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libgl1-mesa-dev libegl1-mesa-dev libgtk-3-dev \
  libudev-dev
```

### macOS

下载得到的是 `.zip`，里面是 `meatshell.app` 应用程序包：

```bash
# 解压(aarch64 = Apple 芯片，x86_64 = Intel)
unzip meatshell-*-macos-*.zip
# 移到「应用程序」(可选，留在原地也行)
mv meatshell.app /Applications/
# 去掉「未签名应用」的隔离属性，否则会提示「meatshell 已损坏，无法打开」
xattr -dr com.apple.quarantine /Applications/meatshell.app
# 打开(或在「访达」里双击)
open /Applications/meatshell.app
```

> 若未移到 `/Applications`，把上面两条路径换成 `.app` 实际所在位置(如 `~/Downloads/meatshell.app`)即可。

> 从源码构建见下方 [运行](#运行)。

## 功能

### 已实现

- [x] FinalShell 风格 UI，深色 / 浅色 / 跟随系统主题
- [x] 本机 + 远端资源监控（CPU / 内存 / 交换 / 网络 / 磁盘）
- [x] 远端进程监控（按 CPU 排序、PID 复制与权限确认后结束进程）
- [x] 完整 VT/ANSI 终端模拟（btop / htop / vim 全屏正常渲染）
- [x] 彩色 emoji（支持肤色、旗帜及 ZWJ 组合序列）
- [x] 多标签页（欢迎页 + 多个会话）
- [x] 会话管理：新建 / 编辑 / 删除 / 分组，本地 JSON 持久化，导出 / 导入（兼容 FinalShell 连接文件）
  - 配置位置：`%APPDATA%/meatshell/sessions.json`（Windows）
    / `~/.config/meatshell/sessions.json`（Linux）
    / `~/Library/Application Support/meatshell/sessions.json`（macOS）
- [x] SSH（`russh`，纯 Rust）：密码 / 私钥 / 加密私钥（密码短语）
- [x] SFTP 文件浏览 + 上传 / 下载（拖拽）+ 终端内 ZMODEM（`sz`）接收
- [x] SSH 端口转发 / 隧道：本地 -L / 远程 -R / 动态 -D（SOCKS5）
- [x] 快捷命令 + 命令输入框（可群发到所有会话）+ 命令历史
- [x] 串口 / Telnet 会话
- [x] 出站代理（SOCKS5 / HTTP）
- [x] 导入 `~/.ssh/config`
- [x] 会话密码加密存储（ChaCha20-Poly1305）
- [x] 已知主机（`known_hosts`）校验 + 首次连接确认
- [x] 多标签页终端分屏
- [x] 多窗口：Ctrl+Shift+N（macOS ⌘⇧N）或系统入口“新建窗口”（Windows 任务栏 / macOS Dock / Linux 桌面右键），Chrome 式单进程管理

彩色 emoji 图形来自 [Twemoji](https://github.com/jdecked/twemoji)，按
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) 使用；完整署名见
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

### 计划中

- [ ] 会话密码改用 OS 钥匙串存储

## 技术栈

| 模块          | 选型                                                              |
| ------------- | ----------------------------------------------------------------- |
| UI            | [Slint](https://slint.dev)（纯 Rust 编译，无 GC）                 |
| 异步运行时    | [`tokio`](https://tokio.rs)                                       |
| SSH 协议      | [`russh`](https://crates.io/crates/russh)（无 libssh 依赖）       |
| 系统指标      | [`sysinfo`](https://crates.io/crates/sysinfo)                     |
| 序列化        | `serde` + `serde_json`                                            |
| 日志          | `tracing` + `tracing-subscriber`                                  |

## 运行

```bash
cargo run --release
```

首次启动会在 `%APPDATA%/meatshell/sessions.json` 建立空的会话库。点击右上
角 **“＋ 新建会话”** 添加第一台服务器。

## CLI 与 MCP 自动化

MeatShell 的 CLI 和 MCP 共用 GUI 中保存的会话及 SSH / SFTP 实现。CLI 适合脚本、
CI 和手动执行明确的命令；MCP 则让支持 MCP 的 AI 客户端通过自然语言完成服务器
巡检、日志分析和文件传输。两者只是调用入口不同，不需要重新维护一份服务器配置。

> 使用前请先在 GUI 中创建并成功连接一次目标会话，以完成主机密钥确认。密码、私钥等
> 凭据不会出现在 CLI/MCP 返回结果中，也不要把明文密码写进提示词或 MCP 配置。

### CLI

查看所有可用命令：

```bash
meatshell cli help
```

常用示例：

```bash
# 列出已保存的会话，第一列是后续命令使用的 session-id
meatshell cli sessions
meatshell cli sessions --json

# 查看单个会话的非敏感信息
meatshell cli session <session-id>

# 执行非交互式 SSH 命令；远端命令必须放在 -- 后面
meatshell cli exec <session-id> -- free -h
meatshell cli exec <session-id> --timeout 60 --json -- journalctl -n 100 --no-pager

# 浏览、读取和传输远端文件
meatshell cli files <session-id> /var/log
meatshell cli read <session-id> /var/log/example.log
meatshell cli upload <session-id> ./local.txt /tmp
meatshell cli download <session-id> /tmp/result.txt ./downloads
```

CLI 的 `<session-id>` 可由 `meatshell cli sessions` 获取。文件下载要求本地目标目录已经
存在，且不会覆盖同名文件。

### MCP

先打开 MeatShell 的 **设置 → 界面 → MCP**：

1. 启用 MCP。
2. 根据需要允许使用已保存的凭据。
3. 需要远程诊断时允许执行任意 SSH 命令。
4. 需要上传或下载文件时允许文件传输。

然后在支持 stdio MCP 的客户端中添加名为 `meatshell` 的服务：

```json
{
  "mcpServers": {
    "meatshell": {
      "command": "/absolute/path/to/meatshell",
      "args": ["mcp", "serve"]
    }
  }
}
```

Windows 下 `command` 可以填写 `C:\\path\\to\\meatshell.exe`。重启或刷新 MCP 客户端
后，应能看到 `meatshell` 服务以及会话查询、远程命令、目录浏览、文本读取、上传和下载
等工具。不同 AI 客户端的 MCP 配置文件位置不同，请以对应客户端文档为准。

#### MCP JSON-RPC 示例

一般情况下由 AI 客户端自动生成这些请求，无需手工输入。调试 stdio 连接时，每个请求
必须是独立的一行 JSON，依次完成初始化和 `notifications/initialized` 通知：

```jsonl
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"example-client","version":"1.0.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
```

查询已保存会话并获取 `<session-id>`：

```jsonl
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sessions","arguments":{}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_session","arguments":{"session_id":"<session-id>"}}}
```

执行 OOM 只读诊断并浏览堆转储目录：

```jsonl
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"run_command","arguments":{"session_id":"<session-id>","command":"free -h; printf '\\n=== kernel OOM ===\\n'; dmesg 2>/dev/null | grep -iE 'oom|out of memory|killed process' | tail -50 || true; printf '\\n=== Java ===\\n'; ps -ef | grep '[j]ava'","timeout_seconds":30,"max_output_bytes":1048576}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list_remote_files","arguments":{"session_id":"<session-id>","path":"/home/jeff/test/heapdumps"}}}
```

读取日志或下载一个堆文件：

```jsonl
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_remote_text_file","arguments":{"session_id":"<session-id>","path":"/home/jeff/test/logs/meatshell-log-demo-error.log"}}}
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"download_file","arguments":{"session_id":"<session-id>","remote_path":"/home/jeff/test/heapdumps/example.hprof","local_directory":"/existing/local/directory","timeout_seconds":120}}}
```

`read_remote_text_file` 只接受有大小和行数限制的 UTF-8 文本；HPROF 等二进制文件应使用
`download_file`。下载目标目录必须已经存在，且工具不会覆盖同名文件。

配置完成后，可以直接对 AI 客户端说：

> 用 `meatshell` MCP 排查一下：我的 `192.168.100.41` 服务器出现 OOM，堆转储位于
> `/home/jeff/test/heapdumps`。请检查系统内存、内核 OOM 记录、Java 进程、应用日志和
> HPROF 文件，判断根因；先只读排查，不要重启服务或删除文件。

MCP 会先通过 `list_sessions` 查找匹配的已保存会话，再按已授予的权限调用远程命令或
SFTP 工具。若存在多条同主机会话，可在提示词中补充 GUI 中的会话名称。建议诊断提示词
明确写出目标主机、日志或堆文件路径，以及是否允许重启、修改配置、下载文件等操作边界。

## 项目布局

```
meatshell/
├── Cargo.toml
├── build.rs                 # Slint 编译器入口
├── ui/
│   ├── app.slint            # 顶层窗口
│   ├── theme.slint          # 设计 tokens
│   ├── widgets.slint        # 可复用按钮 / 输入框 / sparkline
│   ├── sidebar.slint        # 左侧系统监控面板
│   ├── tabs.slint           # 顶部标签栏
│   ├── welcome.slint        # 欢迎页 / 快速连接
│   ├── session_dialog.slint # 新建 / 编辑会话弹框
│   └── terminal_view.slint  # 终端视图（v0.1 行缓冲）
└── src/
    ├── main.rs
    ├── app.rs               # UI ↔ 后端桥接
    ├── config.rs            # 会话 JSON 持久化
    ├── system.rs            # CPU / 内存 / 网络采样
    └── ssh.rs               # SSH 会话 worker
```

## 开发提示

- Slint 控件有非常严格的布局 DSL，改 `.slint` 后 `cargo check` 是最快的
  反馈方式。
- 应用事件循环是单线程（Slint 要求），所有跨线程 UI 更新通过
  `slint::invoke_from_event_loop` 回调。
- SSH / SFTP 共享 `known_hosts` 校验逻辑：首次连接会确认并记住主机密钥，
  后续密钥变化会再次提示。

## 发版

不要直接手动修改 `Cargo.toml` 后再打标签。使用发布脚本，让 Git tag 指向的提交本身就已经包含正确版本号：

```powershell
.\scripts\release.ps1 v0.6.0 -Push
```

脚本会更新 `Cargo.toml` / `Cargo.lock`，运行 `cargo check --locked`，验证 `meatshell --version`，提交 `Release v0.6.0`，创建 annotated tag，并推送当前分支和 tag。更多细节见 [docs/release.md](docs/release.md)。

## 相关群组

<p align="center">
  <img src="docs/QR/QQ_Group_QR_Code.jpg" alt="QQ群二维码" width="300"><br>
  <em>扫描二维码加入 QQ 群，与其他用户交流使用经验、反馈问题或获取最新动态</em>
</p>

## License

MIT OR Apache-2.0（双许可）。
