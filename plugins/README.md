# WB 插件格式 v1

一个插件 = 一个文件夹 + 一个 `plugin.json`。放进 `%LOCALAPPDATA%\WB\plugins\` 即可被加载
（开发期也可直接放在本仓库 `plugins/` 下，daemon 会自动发现）。

## 创建与校验

CLI 可以生成不会覆盖已有目录的 Agent-ready 脚手架。三种形态都会带 `plugin.json` 和 `SKILL.md`；命令型会生成带 UTF-8 BOM 的 PowerShell handler，挂件型会生成可直接渲染的离线 widget：

```text
wb plugin create hello-world --name "Hello World" --kind command
wb plugin create clock-card --name "Clock Card" --kind widget
wb plugin create team-helper --name "Team Helper" --kind hybrid
```

发布前先执行完整校验。它会检查 manifest、权限、重复 id、handler/widget/Skill 是否存在、canonical path 是否留在插件根目录，以及 widget/Skill 的大小和 UTF-8 内容限制：

```text
wb plugin validate path\to\my-plugin --json
wb plugin pack path\to\my-plugin --output my-plugin.zip
```

`pack` 强制复用同一套完整性校验，因此不会产出缺少声明文件的 ZIP。

```text
%LOCALAPPDATA%\WB\plugins\
  hello-assistant\
    plugin.json      ← 清单（必需）
    main.ps1         ← 命令处理器（有 commands 时必需）
    SKILL.md         ← Agent Skill 文档（有 skills 时必需）
  clip-insight\
    plugin.json
    widget.html      ← 面板挂件（有 widget 时必需）
```

## plugin.json

```json
{
  "id": "hello-assistant",        // 小写字母/数字/中划线，建议与文件夹同名
  "name": "Hello 小助手",
  "version": "0.1.0",
  "description": "…",
  "author": "你的名字",
  "handler": "main.ps1",          // 命令处理器，相对本目录
  "commands": [
    {
      "id": "util.hello",          // 命令 id，点分层级，全局唯一
      "title": "打招呼",
      "hint": "一句话说明",
      "arg": { "name": "name", "prompt": "跟谁打招呼？" },   // 可省（无参命令）
      "annotations": {                                        // MCP 标准风险提示
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
      },
      "ai": {                                              // 可省；写了就暴露给 AI 工具
        "description": "给模型看的调用时机说明",
        "properties": { "name": { "type": "string" } },     // JSON Schema properties
        "required": ["name"]
      }
    }
  ],
  "widget": { "file": "widget.html", "title": "剪贴板洞察", "span": 2 },  // 可省；span 1-4
  "skills": [
    {
      "id": "triage",
      "name": "问题分流",
      "description": "告诉 Agent 何时使用这个插件以及如何组合它的能力。",
      "file": "SKILL.md",
      "tags": ["workflow"]
    }
  ],
  "permissions": ["process"]      // commands 必须声明 process；安装后由用户批准
}
```

## 权限与批准

manifest 只接受以下权限；未知项和重复项都会使插件加载失败：

| 权限 | 能力 |
| --- | --- |
| `clipboard.read` | 读取剪贴板历史 |
| `clipboard.write` | 写入或清空剪贴板历史 |
| `data.read` | 读取笔记、待办 |
| `data.write` | 新增、修改或删除笔记和待办 |
| `panel.control` | 显示、隐藏或切换 WB 面板 |
| `network` | 允许 widget 连接 HTTP(S) 或加载网络图片 |
| `filesystem` | 读取应用列表和最近文件；供未来文件能力扩展 |
| `process` | 启动插件 handler；所有命令插件必须声明 |
| `system` | 预留给高风险系统操作；当前 widget RPC 不开放 |

带权限的插件安装后默认处于待批准状态，不会进入普通搜索、命令列表、AI/MCP 工具、Skill 或主看板 widget，也不能执行命令。可在面板插件页批准，或使用 CLI：

```text
wb plugin approve hello-assistant
wb plugin revoke hello-assistant
```

授权写入 `%APPDATA%\WB\settings.json`，并绑定插件版本、排序后的权限集合和 manifest/handler/widget/Skill 文件的 SHA-256 内容指纹。任一文件、版本或权限发生变化，旧授权会自动失效，必须重新审阅并批准当前版本。

`commands[].annotations` 使用 MCP Tool Annotations 的四个标准 Hint，供 Agent 客户端在调用前展示风险和决定是否请求用户确认。未声明时 WB 按最保守值处理：可写、可能破坏、非幂等、可能访问外部世界。它只是提示，不会扩大插件权限，也不能替代版本级批准；插件作者应按 handler 的真实最坏行为填写。

## 命令一旦声明，三处自动可用

| 入口 | 用法 |
| --- | --- |
| 面板（人） | 输入 `>打招呼` 或 `>util.hello`，回车执行 |
| AI（模型） | `?跟 Luna 打个招呼` → 模型自动调 `util_hello` 工具 |
| CLI（外部 Agent） | `wb cmd run util.hello --arg name=Luna` 或 `wb plugin run hello-assistant` |

## handler 契约（进程式，任何语言都行）

- daemon 拉起 handler 子进程，**stdin** 喂一行 JSON：`{"command": "util.hello", "args": {...}}`
- **stdout** 吐一个 JSON 值作为结果（非 JSON 输出会被包成 `{"text": "…"}` 容错）
- stdout/stderr 从启动起并发排空，每路最多保留 1MB；10 秒超时强杀，stderr 内容在失败时作为错误信息
- 解释器按扩展名映射：`.ps1` → Windows PowerShell，`.js` → node，`.py` → python，其余直接执行
- handler/widget/Skill 的 canonical path 必须仍位于插件根目录，目录联接或符号链接也不能越界

PowerShell 最小示例见 `hello-assistant/main.ps1`。

## widget 契约（面板挂件）

- 单文件 HTML（内联 `<style>`/`<script>`），以 sandboxed iframe 装进主看板；插件页只展示安装、批准、撤销和卸载状态
- 内置桥：页面里可调用 `await wbRpc('clip.get', { last: 5 })`；父页会把真实插件身份交给 daemon 权限网关
- 背景必须透明（卡片玻璃底由面板提供）；字体/颜色参考 `clip-insight/widget.html`
- 大小上限 256KB；默认 CSP 禁止外联，只有声明并获批 `network` 后才开放 HTTP(S) connect/image

`widget.span` 使用主看板的 6 列网格（1-4 列）。widget 获批后会和内置组件一起参与显隐定制；撤销批准或插件内容指纹变化后，组件立即从主看板移除，重新批准当前版本才会恢复。

widget RPC 是显式白名单，不是 daemon 任意方法透传：

| 方法 | 所需权限 |
| --- | --- |
| `clip.get` | `clipboard.read` |
| `clip.add`, `clip.clear` | `clipboard.write` |
| `note.list`, `note.get`, `todo.list` | `data.read` |
| `note.add`, `note.rm`, `todo.add`, `todo.done`, `todo.rm` | `data.write` |
| `apps.list`, `recent.list` | `filesystem` |
| `panel.show`, `panel.hide`, `panel.toggle` | `panel.control` |

未列出的调用会被拒绝，即使插件声明了其他权限也不会放行。

## 安全边界

权限网关限制插件何时可见、widget 能调用哪些 WB 能力，但 `process` **不是操作系统沙箱**。获批的 handler 是当前 Windows 用户权限下的本地代码，可以直接访问该用户本来能访问的文件、网络和进程；请只批准你信任并审阅过的插件。

AI 侧只能调用 manifest 里显式带 `ai` 的命令；高风险命令不要写 `ai` 段。命令/工具 id 还必须在全局注册表中唯一，插件不能覆盖内建命令、内建 AI 工具、`skill_list` / `skill_get` 或其他插件。

改动插件后：`wb plugin reload`。命令池会立即刷新，面板每 3 秒检查 revision，也可在插件页手动刷新；内容指纹变化后需重新批准。仓库内 [`stopwatch`](stopwatch/) 是首个无权限官方 widget，用于验证“插件组件进入主看板”的完整链路。

## 打包与安装

插件目录可直接打成 ZIP，再交给其他用户安装：

```text
wb plugin pack path\to\my-plugin --output my-plugin.zip
wb plugin validate path\to\my-plugin --json
wb plugin install my-plugin.zip
wb plugin list
wb plugin remove my-plugin
```

`pack` 的 JSON 结果包含可发布的 `sha256:<hex>`。从 HTTP(S) 安装时校验值必填：

```powershell
wb plugin install https://plugins.example/my-plugin.zip `
  --sha256 sha256:<pack 输出的哈希>
```

远程归档上限 32MB，解压后上限 64MB / 512 个文件 / 16 层目录。安装器逐项解析 ZIP，在写盘前/写盘中拒绝越界路径、符号链接、NTFS ADS、Windows 设备名、大小写冲突和超限内容。下载、校验和解压都在正式插件目录之外完成；升级提交失败会回滚旧版本，极端回滚失败时旧版本会保留在 `%LOCALAPPDATA%\WB\plugin-backups\` 并返回具体路径。公开分发应使用 HTTPS，HTTP 仅用于本地开发服务器。

## 市场索引 v1

官方或社区可以托管同一种静态 `index.json`，格式由 [`market-index.schema.json`](market-index.schema.json) 定义。每个 id 在单个索引中只能出现一次，`version` 必须是 SemVer，`sha256` 来自 `wb plugin pack`：

```json
{
  "schema_version": 1,
  "name": "WB Community",
  "plugins": [{
    "id": "my-plugin",
    "name": "My Plugin",
    "version": "1.2.0",
    "description": "示例插件",
    "author": "community",
    "download": "https://plugins.example/my-plugin-1.2.0.zip",
    "sha256": "sha256:<64 位十六进制>",
    "homepage": "https://plugins.example/my-plugin",
    "tags": ["productivity"]
  }]
}
```

远程索引中的 `download` 必须是绝对 HTTP(S) URL；本地索引可以引用索引目录内的相对 ZIP，也可以引用 HTTP(S)。可以持久化最多 8 个官方或社区市场源；不传 `--index` 时会聚合全部已配置来源：

```powershell
wb plugin market source add https://plugins.example/index.json
wb plugin market source list
wb plugin market list
wb plugin market check
wb plugin market install my-plugin
wb plugin market update my-plugin
wb plugin market source remove https://plugins.example/index.json
```

也可以给 `list|check|install|update` 传 `--index <path-or-url>`，只访问一次指定索引。多个已配置来源含有同一个插件 id 时，自动解析会拒绝歧义，需用 `--index` 选择来源。面板插件页提供同一套市场浏览、来源管理、安装与更新能力。

市场元数据不替代包内 manifest。安装提交前会同时核对归档 SHA-256、插件 id 和版本；任一不一致都不会覆盖现有版本。升级后原有授权按版本和内容指纹自动失效，需要重新批准。

安装会校验 manifest 和所有声明文件、复制到 `%LOCALAPPDATA%\WB\plugins\` 并立即刷新 daemon 插件池；同 id 的用户插件会覆盖仓库开发态插件。卸载只删除用户插件，不会修改仓库里的开发插件。

AI 面板会把 `skill_list` / `skill_get` 作为工具提供给模型。模型可以先读取插件 Skill，再调用同一插件声明的命令；Skill 本身只提供上下文，不直接执行代码。

## Agent Skill

`skills` 是插件随附的 Markdown/纯文本能力说明。daemon 提供 `skill.list` 和 `skill.get`，CLI 对应：

```text
wb skill list
wb skill get hello-assistant greeting
```

Skill 只提供可审阅的上下文，不直接执行代码；执行仍统一走 `cmd.run` 或插件命令。这样同一个插件可以同时开放小组件、命令和 Agent 工作流说明。
