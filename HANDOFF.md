# WB 项目交接文档（2026-08-22 早）

> 新 session 开场直接说：「读 E:\cctest\wb\HANDOFF.md，继续 WB 开发」即可。
> 本文档 = 项目现状 + 操作纪律 + 下一步。改代码前先把「操作纪律」读完，全是血泪。

## 1. 项目是什么

WB（Windows Bar / WorkBench）：**Agent-Native 的 Windows 桌面入口**，对标并超越 DeskBox / uTools。
- 人：按 **Win 键**呼出全屏磨砂面板（替代开始菜单语义）——搜索应用/文件/剪贴板/笔记、`?` 问 AI、`>` 跑命令、小组件看板（三页：组件 ⇄ 程序坞 ⇄ 插件）。
- Agent：`wb.exe` CLI（`--json` 契约）+ 命名管道 daemon；AI 模型经 function calling 直接操作面板能力。
- 生态：**插件格式 v1**（M4 刚落地）——小组件和 Agent 命令统一成标准插件，社区可自制。

技术栈：Rust workspace（GNU 工具链）+ WebView2 前端（原生 HTML/JS，无框架）。设计文档 `E:\cctest\docs\技术方案-v0.1.md`，格式文档 `plugins/README.md`，里程碑记录全在 `README.md`。

## 2. 仓库结构

```
E:\cctest\wb\
  Cargo.toml            # workspace
  build.sh              # 工具链环境（Git Bash 下 source）
  README.md             # 里程碑流水账（v1→v11、M1→M4），继续往里写
  assets\panel-ui\index.html   # 面板全部前端（单文件，~1500 行）
  crates\
    wb-core\            # 模型/存储(sqlite)/搜索/协议/命令注册表commands.rs/ai.rs(ask_sync)
    wb-daemon\          # 常驻 JSON-RPC（命名管道 wb-daemon）；clipboard 监听；panelctl；插件装配
    wb-cli\             # wb.exe（注意：bin 名是 wb，不是 wb-cli）
    wb-hook\            # Win 键低级钩子（wb-hook-poc.exe）
    wb-panel\           # 面板宿主：host.rs/ipc.rs/ai.rs(流式+function calling)/dwm.rs(磨砂)/webview2.rs
    wb-plugin-sdk\      # 插件 manifest 类型+校验（5 单测）
    wb-plugin-host\     # 插件发现/进程执行/挂件读取（2 单测）
    wb-mcp\             # stub，未做
  plugins\              # 开发态插件目录（daemon 自动发现）：hello-assistant、clip-insight
  docs-assets\          # 验证截图
  target\debug\         # 产物：wb.exe / wb-daemon.exe / wb-panel.exe / wb-hook-poc.exe
```

## 3. 操作纪律（每条都踩过坑）

1. **编译**：Git Bash 里 `cd /e/cctest/wb && source build.sh && cargo build`。rustc 1.98.0 GNU，链接器在仓库 `.toolchain/mingw64`。
2. **编译前必须 `taskkill //F //IM wb-panel.exe` 和 `wb-daemon.exe`**，否则链接 Permission denied（exe 被占）。
3. **启动面板必须带 `--wv2`**，否则建的是无 WebView2 的透明空窗。
4. **测试启动面板用 PowerShell Start-Process 且必须带 `-RedirectStandardOutput`**：
   - 不带重定向会弹出黑色控制台窗口（console 子系统），它会被 veil 磨砂采样成"大黑块"——已因此误判过一次 bug。
   - Bash 工具调用结束会收割 `&` 后台子进程；Start-Process 起的进程能存活。所有「启动+等待+截图」要在一个 Bash 调用内完成。
5. **daemon 不用手动管**：任意 `wb` 命令会自动拉起；但改了 daemon 代码要先 taskkill 再用新二进制。
6. **PowerShell 插件脚本必须存成 UTF-8 with BOM**（PS 5.1 无 BOM 按 GBK 解析会炸字符串）；宿主已强制控制台 UTF-8 读写，插件作者不用管管道编码。
7. 全路径：`/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe`。截图脚本 `/tmp/shot2.ps1 -name xxx`（DPI 感知 2560x1440 → `docs-assets/xxx.png`，回复里用 `![x](E:\cctest\wb\docs-assets\xxx.png)` 给用户看）。探针 `/tmp/probe2.ps1`（面板窗口可见性）。
8. 用户常在用机器（打游戏/浏览），**测试面板一律带 `--no-autohide`** 否则失焦自动藏。
9. **页面测试钩子**（hash）：`#test-ai`（纯问答）/ `#test-ai2`（工具调用：加待办）/ `#test-ai3`（插件工具 util_hello）/ `#test-cmd`（> 命令模式）/ `#view=apps` / `#view=plugins` / `#dbg-rects` / `#dbg-top`（矩形转储到 stdout）。AI 结果经 selftest 回显宿主 stdout：`grep '"detail":"ai.done' target/xxx.log`。
10. **改 index.html 不用编译**；改完跑语法检查：
    `node -e "const s=require('fs').readFileSync('assets/panel-ui/index.html','utf8'); const m=s.match(/<script>([\s\S]*)<\/script>/); new Function(m[1]); console.log('ok')"`
    注意：内联 `<script>` 里写字符串形式的 `</script>` 必须用 `<\/script>` 或拼接（PLUGIN_SHIM 里有范例）。
11. **AI 配置**：`E:\cctest\api.json` 有中转站信息；模型用 **gpt-5.6-luna**（api.json 里写的别的型号是别的工具的配置，以用户说的为准）。网络问题走代理 `127.0.0.1:7890`（代码已内置网络错误自动代理重试一次）。中转站支持 Responses API + tools/function calling（已实测）。
12. 测试产生的待办/笔记数据测完随手清掉（`wb todo rm`）。
13. 用户正常使用的实例：wb-hook-poc.exe（保留单实例！他靠它按 Win）+ 正式 wb-panel（`--wv2`，无测试旗标）。测试完务必恢复这个状态。taskkill 面板不会动钩子。
14. 回复用户用中文、简洁；结尾一条**加粗**的下一步建议。用户审美要求高——UI 改动要截图自审，对标 DeskBox（参考项目在 `E:\cctest\deskbox-ref`）。

## 4. 架构要点（新人 5 分钟版）

- **单一事实源 = wb-daemon**（JSON-RPC over 命名管道）。面板/CLI/将来的 MCP 都是平等客户端。方法表见 `wb-core/src/protocol.rs` schema()。
- **命令注册表**（`wb-core/src/commands.rs`）：10 条内建命令。同一份数据三处用：`list_json()`（页面 `>` 模式 + CLI）、`tools_json()`（AI function calling）、方法名直映射（cmd.run 转发）。破坏性命令（clip.clear/system.lock）不暴露给 AI。
- **插件**（M4）：daemon 启动时发现 `%LOCALAPPDATA%/WB/plugins` + 仓库 `plugins/`；`cmd.list`/`cmd.tools` 自动合并插件命令（source:"plugin"）；执行 = 进程式（stdin JSON → stdout JSON，10s 超时）。AI 工具名规则：点换下划线互逆（`util.hello` ⇔ `util_hello`），面板 ai.rs 的 exec_tool 一律走 `cmd.run` 不分辨内建/插件。挂件 = sandboxed iframe + srcdoc + `wbRpc` 桥（iframe postMessage → 父页中继 → daemon）。
- **面板视觉**：v11 整屏 veil 磨砂（dwm.rs）：24 张亚克力窗的池，只用第 1 张拉满工作区钉在面板下，显隐各一次定位+淡入淡出，动画期零窗口操作（165Hz 下逐卡跟踪会卡，别回去；WB_CARD_FROST=1 可 A/B）。显隐动画协议：页面先冻结在第 0 帧（hold）→ 宿主亮窗发 "go" → 动画起点即亮窗帧（消灭"先闪一下"）。
- **AI 链路**：面板 `?` → ai.rs 起线程 curl SSE 流式（系统 curl.exe 零依赖，body 走 stdin）；function calling 回合制最多 3 轮、末轮不带 tools 强制纯文本收尾；daemon 侧 `agent.ask` 走 `wb-core::ai::ask_sync`（非流式）。

## 5. 当前状态：M4 已验证，M5 已起步

- M1 内核 / M2 面板 / v11 磨砂 / M3 随手问流式 / M3.5 命令注册表+function calling / M4 插件系统。
- M4 实测：`wb cmd run util.hello --arg name=WB` 中文无乱码；`?跟 Luna 打个招呼` → 模型自动调插件 `util_hello` → 确认；插件页挂件渲染 + wbRpc 桥读剪贴板统计正常。
- M5 第一阶段：插件 manifest 支持 `skills` 文档；daemon 暴露 `skill.list` / `skill.get`、`plugin.install` / `plugin.remove`，CLI 提供 `wb skill ...`、`wb plugin pack/install/remove`；ZIP/目录安装会校验、复制到用户目录并立即刷新 daemon 插件池；面板 AI 增加 `skill_list` / `skill_get` 工具。插件页现在每 3 秒检查清单 revision，新增/删除/替换挂件会自动重建 iframe，另有手动刷新按钮。`hello-assistant` 已带 `SKILL.md` 示例。真实 CLI 安装/执行/卸载冒烟、插件页新增挂件截图验证通过，workspace 测试全绿。
- M5 Agent 层：`wb-mcp.exe` 已从 stub 升级为 stdio MCP server，支持 `initialize`、`tools/list`、`tools/call`、`resources/list`、`resources/read`、`ping`；内建/插件命令从 daemon `cmd.tools` 映射，Skill 以 `wb://skill/<plugin>/<id>` resource 暴露。协议级冒烟已验证能看到 `util_hello`、`skill_list` 并读取 Skill。
- M5 入口设置：`settings.get` / `settings.set` / `hook.status` 已接入 daemon，面板 ⚙ 弹层和 CLI `wb settings get|win|autostart` 可控制 Win 键接管及 HKCU Run 开机自启；daemon 按设置启动 hook，hook 通过 `Local\\WBHookSingleInstance` 保证单实例。真实测试已验证关闭/开启接管、注册表创建/删除。
- cargo test：wb-core 6 + wb-plugin-sdk 5 + wb-plugin-host 2 全绿。

## 6. 已知瑕疵 / 未验证声明

- 面板无单例保护（测试时记得杀干净，否则会起多个）。
- Start-Process 不带重定向会出控制台黑窗——生产路径（daemon panelctl 拉起）已用 CREATE_NO_WINDOW，无此问题。
- 插件挂件支持面板内自动热加载（3 秒轮询 revision）和插件页手动刷新；插件代码仍在 iframe 创建时加载，修改后等待下一轮检查或点刷新。
- 插件权限仅声明不强制（v1 信任模型 = 用户自装本地代码）。
- `events.tail` 未实现；`daemon stop` 未实现（用 taskkill）。MCP 当前为单进程 stdio 会话，每个 MCP server 连接独立复用一个 daemon pipe。
- Everything（voidtools）文件搜索未接入，daemon 启动时只检测并打印是否存在。
- 托盘尚未做；开机自启已通过设置页和 HKCU Run 完成。

## 7. 建议的下一步（按用户愿景排序）

1. **M5 插件生态深化**：插件设置页、把内置 16 个组件逐步迁移成插件格式自证、插件市场/版本升级。Skill 读取、AI 上下文选择和挂件热加载已接入。
2. **Agent 生态深化**：MCP 已接通，下一步补外部 Agent 配置样例、事件订阅和更细的权限/确认策略。
3. **Everything 搜索接入**（WM_COPYDATA 客户端）——文件搜索从"本地存储"升级"全盘毫秒级"。
4. 托盘常驻 + `daemon stop`。
5. 更多内置插件候选：天气城市切换、二维码生成、颜色拾取、SSH/Hosts 快捷。

## 8. 上次会话最后在做的事

M4 刚完成并回复了用户（附 m4-ai-plugin.png / m4-plugins-page.png 截图）。给用户的下一步建议是插件生态深化（M5）。用户尚未回复是否同意。
