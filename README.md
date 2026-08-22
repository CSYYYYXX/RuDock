# RuDock

<p align="center">
  <strong>按一次 Win 键，打开属于你的 Windows 工作台。</strong><br>
  <sub>统一搜索 · AI 随手问 · 桌面小组件 · 插件生态 · CLI / MCP</sub>
</p>

<p align="center">
  <a href="https://github.com/CSYYYYXX/RuDock"><img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-4CC2FF?style=flat-square" alt="Windows 10 / 11"></a>
  <a href="https://github.com/CSYYYYXX/RuDock"><img src="https://img.shields.io/badge/status-preview-F9C74F?style=flat-square" alt="Preview"></a>
  <a href="https://github.com/CSYYYYXX/RuDock/blob/main/plugins/README.md"><img src="https://img.shields.io/badge/plugins-v1-6CCB5F?style=flat-square" alt="Plugin format v1"></a>
  <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/MCP-ready-BD93F9?style=flat-square" alt="MCP ready"></a>
</p>

![RuDock 主看板](docs-assets/v8-final.png)

## 把 Win 键变成个人工作台

RuDock 是一个可定制的 Windows 桌面入口。它把应用、文件、剪贴板、笔记、待办、AI 和插件工具放进同一个面板：需要时按一下 **Win**，用完按 **Esc**，不必离开正在进行的工作。

RuDock 同时面向普通用户、自动化脚本和 AI Agent：人在面板里操作，脚本通过 CLI 调用，Agent 通过 MCP 使用相同的能力。

## 你可以用它做什么

| 功能 | 体验 |
| --- | --- |
| 统一搜索 | 搜索应用、文件和个人内容，查看来源与详情，并直接打开、定位或复制 |
| AI 随手问 | 输入 `?` 开始流式问答，让 AI 查询信息或操作已授权的工具 |
| 即时工具 | 输入 `=` 计算表达式，输入 `>` 执行命令 |
| 个性看板 | 自由组合天气、日历、时钟、媒体、系统状态、待办、随手记等小组件，并选择常驻桌面的内容 |
| 插件扩展 | 安装命令、组件和 Skill，为面板、CLI 与 Agent 添加新能力 |
| 快速呼出 | 应用索引随后台服务预先建立，打开面板后可以直接搜索 |

## 基本操作

| 操作 | 结果 |
| --- | --- |
| 按 `Win` | 打开或关闭 RuDock |
| 直接输入 | 搜索应用、文件和个人内容 |
| 输入 `? 问题` | 进入 AI 模式 |
| 输入 `= 表达式` | 本地计算 |
| 输入 `> 命令` | 查找并执行命令 |
| 按 `Ctrl+K` 或 `→` | 进入所选结果的动作区 |
| 按 `Esc` | 关闭面板，回到当前工作 |

Win 键接管可以随时在设置中关闭。关闭后仍可从托盘图标或 CLI 打开 RuDock。

## 桌面常驻组件

天气、日历、时钟和 AI 等组件可以脱离面板，常驻在 Windows 桌面上。它们只占用卡片自身的区域；打开其他软件时仍按正常窗口层级工作，不会变成覆盖所有窗口的悬浮层。

![RuDock 桌面常驻组件](docs-assets/desktop-widgets.png)

在设置的 **桌面常驻** 中勾选需要的组件即可。按 Win 键时，RuDock 默认进入应用页；切换到左侧一页可以查看完整组件页。CLI 也可以直接管理常驻组件：

```powershell
wb settings desktop w-clock w-weather w-cal w-ai
wb settings desktop
```

第二条命令会关闭全部桌面常驻组件。

## AI Spotlight

AI 模式使用流式回答，并能在获得授权后操作待办、笔记和插件命令。输入框会以轻量动态光路提示当前状态；系统启用“减少动态效果”时，RuDock 也会同步减少动画。

![AI Spotlight 动态光效](docs-assets/ai-spotlight-glow.png)

AI 服务配置只保存在本机。请不要把 API Key 或本地配置文件提交到仓库。

## 小组件与插件

小组件可以直接进入主看板，命令可以被搜索、AI、CLI 和 MCP 共同调用。插件在启用前会展示所需权限；撤销批准后，其组件和命令会立即停止加载。

![插件管理与主看板](docs-assets/m5-plugins-installed-uninstall.png)

插件市场支持安装、升级和卸载，并通过 SHA-256 校验下载内容：

![插件市场](docs-assets/m5-market-page.png)

插件开发文档、manifest 示例与 widget RPC 接口见 [`plugins/README.md`](plugins/README.md)。

## 安装与启动

RuDock 目前处于 Preview 阶段，暂时需要从源码构建。正式安装包将在后续版本发布到 [Releases](https://github.com/CSYYYYXX/RuDock/releases)。

### 环境要求

- Windows 10 / 11
- WebView2 Runtime
- Rust GNU toolchain
- PowerShell 5.1 或更高版本
- Everything（可选，用于扩大文件搜索范围并提升速度）

### 从源码构建

在 Git Bash 中执行：

```bash
git clone https://github.com/CSYYYYXX/RuDock.git
cd RuDock
source build.sh
cargo build --workspace
```

### 启动 RuDock

在 PowerShell 中执行：

```powershell
.\target\debug\wb.exe daemon start
```

首次启动后，在设置中开启 **接管 Win 键**。也可以使用 CLI：

```powershell
.\target\debug\wb.exe settings win true
.\target\debug\wb.exe settings autostart true
```

后台服务会管理面板、托盘图标和 Win 键监听。退出时可使用托盘菜单，或运行：

```powershell
.\target\debug\wb.exe daemon stop
```

## CLI 与 Agent

RuDock 的 CLI 支持结构化 JSON 输出，可直接用于 PowerShell、自动化脚本和 Agent：

```powershell
wb search "发布" --json
wb cmd list --json
wb cmd run todo.add --arg title="检查发布" --json
wb skill list --json
```

需要接入 Claude、Cursor、Codex 或其他 MCP 客户端时，可以生成对应配置：

```powershell
wb mcp config claude
wb mcp config cursor
wb mcp config codex
wb mcp config generic
```

完整命令、MCP 能力和接入方式见 [`AGENT_INTEGRATION.md`](AGENT_INTEGRATION.md)。

## 创建插件

RuDock 插件可以同时提供面板命令、看板组件和 Agent Skill：

```powershell
wb plugin create clock-card --name "Clock Card" --kind widget
wb plugin validate .\clock-card --json
wb plugin pack .\clock-card --output clock-card.zip
wb plugin install .\clock-card.zip
wb plugin approve clock-card
```

插件能力按需授权，授权与版本及内容指纹绑定。widget 运行在受限 iframe 中，只能调用 manifest 明确声明的接口。

## 安全说明

- 未批准的插件不会进入搜索、AI、MCP 或主看板。
- widget 默认禁止外部网络访问，只能调用白名单 RPC。
- 带有 `process` 权限的插件会以当前 Windows 用户权限运行，请只批准可信代码。
- Everything 不可用时，文件搜索会自动回退到常用目录索引。
- AI Key 和个人数据保存在本机，不应提交到 Git 仓库。

## License

RuDock 仍处于 Preview 阶段，许可证将在首个公开发行版前确定。
