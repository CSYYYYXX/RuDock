# RuDock (WB)

<p align="center">
  <strong>按一次 Win 键，打开属于你的 Windows 工作台。</strong><br>
  <sub>Spotlight 搜索 · AI 随手问 · 小组件看板 · CLI / MCP · 可扩展插件生态</sub>
</p>

<p align="center">
  <a href="https://github.com/CSYYYYXX/RuDock"><img src="https://img.shields.io/badge/platform-Windows%2011-4CC2FF?style=flat-square" alt="Windows 11"></a>
  <a href="https://github.com/CSYYYYXX/RuDock/actions"><img src="https://img.shields.io/badge/tests-58%20passing-6CCB5F?style=flat-square" alt="58 tests passing"></a>
  <a href="https://github.com/CSYYYYXX/RuDock/blob/main/plugins/README.md"><img src="https://img.shields.io/badge/plugins-v1-F9C74F?style=flat-square" alt="Plugin format v1"></a>
  <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/MCP-ready-BD93F9?style=flat-square" alt="MCP ready"></a>
</p>

![RuDock 主看板](docs-assets/v8-final.png)

## 它是什么

RuDock（内部代号 WB，Windows Bar / WorkBench）是一个面向 **人和 Agent** 的 Windows 桌面入口：

- 人按 **Win** 键呼出全屏玻璃面板，搜索应用、文件、剪贴板、笔记和待办。
- 输入 `?` 随手问 AI，回答可以流式显示，也可以通过 function calling 操作待办、笔记和已批准的插件命令。
- 输入 `=` 做本地计算，输入 `>` 执行命令；按 `Esc` 关闭，工作流不被打断。
- 小组件、命令和 Agent Skill 共享同一套插件格式，社区可以把能力装进看板，也可以装进 CLI / MCP。

RuDock 的目标不是再做一个启动器，而是把 Windows 键变成一个可编排的个人入口：**人用面板，脚本用 CLI，Agent 用 MCP，插件一次开发三处可用。**

## 现在能做什么

| 区域 | 能力 |
| --- | --- |
| Spotlight | 应用、Everything 文件搜索、剪贴板、最近文件、笔记、待办、插件命令统一检索 |
| AI | `?` 前缀进入流式问答；支持内建工具、Skill 读取和已批准插件 function calling |
| 看板 | 时钟、天气、日历、世界时钟、媒体、系统状态、专注计时、计算器、剪贴板、待办、随手记、快捷启动、最近文件、一言 |
| 程序坞 | 应用索引在 daemon 启动阶段预热，Win 键呼出后直接展示，不在交互热路径启动 PowerShell |
| 插件 | command / widget / hybrid；权限批准、内容指纹、沙箱 iframe、市场和离线校验 |
| Agent | `wb.exe --json`、`wb-mcp.exe`、Skill resources、MCP 动态目录通知、写操作策略和审计事件 |

### AI 输入框

AI 模式会把输入框的边缘光路提升为更快、更亮的青紫呼吸效果；普通搜索保持低亮度慢速流动。边框厚度不变，支持 `prefers-reduced-motion`。

![AI Spotlight 动态光效](docs-assets/ai-spotlight-glow.png)

### 插件组件

首个官方 dogfood 组件是 [`plugins/stopwatch`](plugins/stopwatch)：它没有权限，批准后直接进入主看板；插件管理页只负责批准、撤销和卸载。widget 运行在 sandboxed iframe 中，通过显式白名单 RPC 与 RuDock 通信。

![插件管理与主看板](docs-assets/m5-plugins-installed-uninstall.png)

插件市场支持本地或远程索引、SHA-256 校验、安装、升级和卸载：

![插件市场](docs-assets/m5-market-page.png)

## 快速开始

### 环境

- Windows 10/11，WebView2 Runtime
- Rust GNU toolchain（仓库提供 `.toolchain` 和 `build.sh`）
- PowerShell 5.1 或更高版本
- Everything 可选；未运行时自动降级到常用目录索引

### 构建

在 Git Bash 中：

```bash
git clone https://github.com/CSYYYYXX/RuDock.git
cd RuDock
source build.sh
cargo test --workspace
cargo build --workspace
```

产物位于 `target/debug/`：`wb.exe`、`wb-daemon.exe`、`wb-panel.exe`、`wb-hook-poc.exe`、`wb-mcp.exe`。

### 启动

```powershell
.\target\debug\wb-daemon.exe
.\target\debug\wb-panel.exe --wv2
.\target\debug\wb-hook-poc.exe --panel
```

生产运行建议让 daemon 管理 panel 和 hook；调试面板可加 `--no-autohide`，避免截图或调试时失焦自动关闭。

## CLI / MCP

CLI 输出 JSON 时保持机器可读，适合 shell、自动化和 Agent：

```powershell
wb daemon start
wb search "发布" --json
wb cmd list --json
wb cmd run todo.add --arg title="检查发布" --json
wb skill list --json
wb events --after 0 --limit 50 --json
```

MCP 配置可直接由 CLI 生成：

```powershell
wb mcp config claude
wb mcp config cursor
wb mcp config codex
wb mcp config generic
```

`wb-mcp.exe` 支持 `tools/list`、`tools/call`、`resources/list`、`resources/read` 和 `list_changed` 通知。设置中可选择 MCP 写策略：沿用客户端确认、每次 elicitation 询问、或服务端只读。详细接入示例见 [`AGENT_INTEGRATION.md`](AGENT_INTEGRATION.md)。

## 插件生态

用 CLI 生成 Agent-ready 插件骨架：

```powershell
wb plugin create clock-card --name "Clock Card" --kind widget
wb plugin validate .\clock-card --json
wb plugin pack .\clock-card --output clock-card.zip
wb plugin install .\clock-card.zip
wb plugin approve clock-card
```

一个插件可以同时声明：

- `commands`：面板 `>`、AI 工具、CLI `cmd.run`
- `widget`：主看板中的独立组件
- `skills`：供 Agent 读取的 Markdown 能力说明
- `permissions`：按能力最小化授权，授权绑定版本和内容 SHA-256

完整 manifest、widget RPC 白名单、市场索引 schema 和安全边界见 [`plugins/README.md`](plugins/README.md)。

## 架构速览

```text
Win 键 / 托盘 / CLI / MCP
            │
            ▼
      wb-daemon（单一事实源）
       │ JSON-RPC over Named Pipe
       ├── wb-core：模型、存储、搜索、命令注册表、AI
       ├── wb-plugin-host：发现、校验、权限、事务安装
       ├── wb-panel：原生窗口 + WebView2 + DWM 玻璃
       └── wb-mcp：stdio MCP + Skill resources
```

面板前端是单文件原生 HTML/CSS/JS，没有前端框架依赖。卡片使用 DWM 磨砂池与实时矩形对位，空隙保留桌面动态；动画遵循“冻结首帧 → 宿主亮窗 → 页面解冻”的握手，避免闪现。

## 安全与边界

- 未批准的带权限插件不会进入搜索、AI、MCP 或看板。
- widget 默认 CSP 禁止外联，RPC 只允许显式白名单方法。
- `process` 权限不是操作系统沙箱；获批 handler 以当前 Windows 用户权限运行，只批准你信任的代码。
- Everything 只负责提升文件覆盖和速度；不可用时会回退到 Desktop、Documents、Downloads、OneDrive 的有界索引。
- API 配置只应保存在本机，不要把 `api.json` 或任何密钥提交到仓库。

## 文档

- [`AGENT_INTEGRATION.md`](AGENT_INTEGRATION.md)：CLI、MCP、Claude、Cursor、Codex 接入
- [`plugins/README.md`](plugins/README.md)：插件格式、Skill、widget、权限和市场
- [`HANDOFF.md`](HANDOFF.md)：开发纪律、验证入口和当前交接状态
- [`docs-assets/`](docs-assets/)：UI 与链路验证截图

## 当前验证

2026-08-22 基线：workspace **58 项单测通过**，`cargo build --workspace` 通过；应用索引在 daemon 启动阶段同步建立，本机快照 341 项。AI、MCP 动态目录、Everything 在线/离线降级、插件安装升级回滚和 Win 键生命周期均有本机验证记录。

## License

项目仍处于快速迭代阶段，许可证和正式发布包将在首个公开发行版前补齐。
