# WB 插件格式 v1

一个插件 = 一个文件夹 + 一个 `plugin.json`。放进 `%LOCALAPPDATA%\WB\plugins\` 即可被加载
（开发期也可直接放在本仓库 `plugins/` 下，daemon 会自动发现）。

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
  "permissions": []               // v1 仅声明不强制：network / fs / clipboard
}
```

## 命令一旦声明，三处自动可用

| 入口 | 用法 |
| --- | --- |
| 面板（人） | 输入 `>打招呼` 或 `>util.hello`，回车执行 |
| AI（模型） | `?跟 Luna 打个招呼` → 模型自动调 `util_hello` 工具 |
| CLI（外部 Agent） | `wb cmd run util.hello --arg name=Luna` 或 `wb plugin run hello-assistant` |

## handler 契约（进程式，任何语言都行）

- daemon 拉起 handler 子进程，**stdin** 喂一行 JSON：`{"command": "util.hello", "args": {...}}`
- **stdout** 吐一个 JSON 值作为结果（非 JSON 输出会被包成 `{"text": "…"}` 容错）
- 10 秒超时强杀；stderr 内容在失败时作为错误信息
- 解释器按扩展名映射：`.ps1` → Windows PowerShell，`.js` → node，`.py` → python，其余直接执行

PowerShell 最小示例见 `hello-assistant/main.ps1`。

## widget 契约（面板挂件）

- 单文件 HTML（内联 `<style>`/`<script>`），以 sandboxed iframe 装进面板第三页「插件」页
- 内置桥：页面里直接 `await wbRpc('clip.get', { last: 5 })` 即可调 daemon 的任意方法
- 背景必须透明（卡片玻璃底由面板提供）；字体/颜色参考 `clip-insight/widget.html`
- 大小上限 256KB；别访问外网图片（离线优先）

## 安全边界

插件是你自己安装的本地代码，与安装普通软件同权——请只装你信任的插件。
AI 侧只能调用你在 manifest 里显式写了 `ai` 的命令；破坏性命令不要写 `ai` 段。

改动插件后：`wb plugin reload`（命令立刻生效；面板挂件在面板下次启动时加载）。

## 打包与安装

插件目录可直接打成 ZIP，再交给其他用户安装：

```text
wb plugin pack path\to\my-plugin --output my-plugin.zip
wb plugin install my-plugin.zip
wb plugin list
wb plugin remove my-plugin
```

安装会校验 manifest、复制到 `%LOCALAPPDATA%\WB\plugins\` 并立即刷新 daemon 插件池；同 id 的用户插件会覆盖仓库开发态插件。卸载只删除用户插件，不会修改仓库里的开发插件。

AI 面板会把 `skill_list` / `skill_get` 作为工具提供给模型。模型可以先读取插件 Skill，再调用同一插件声明的命令；Skill 本身只提供上下文，不直接执行代码。

## Agent Skill

`skills` 是插件随附的 Markdown/纯文本能力说明。daemon 提供 `skill.list` 和 `skill.get`，CLI 对应：

```text
wb skill list
wb skill get hello-assistant greeting
```

Skill 只提供可审阅的上下文，不直接执行代码；执行仍统一走 `cmd.run` 或插件命令。这样同一个插件可以同时开放小组件、命令和 Agent 工作流说明。
