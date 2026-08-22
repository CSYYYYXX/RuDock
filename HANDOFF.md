# WB 项目交接文档（2026-08-23）

> 新 session 开场直接说：「读 E:\cctest\wb\HANDOFF.md，继续 WB 开发」即可。
> 本文档 = 项目现状 + 操作纪律 + 下一步。改代码前先把「操作纪律」读完，全是血泪。

## 1. 项目是什么

WB（Windows Bar / WorkBench）：**Agent-Native 的 Windows 桌面入口**，对标并超越 DeskBox / uTools。
- 人：按 **Win 键**呼出全屏磨砂面板（替代开始菜单语义）——搜索应用/文件/剪贴板/笔记、`?` 问 AI、`>` 跑命令、小组件看板（三页：组件 ⇄ 程序坞 ⇄ 插件）。
- Agent：`wb.exe` CLI（`--json` 契约）+ 命名管道 daemon；AI 模型经 function calling 直接操作面板能力。
- 生态：**插件格式 v1**（M4 刚落地）——小组件和 Agent 命令统一成标准插件，社区可自制。

技术栈：Rust workspace（GNU 工具链）+ WebView2 前端（原生 HTML/JS，无框架）。设计文档 `E:\cctest\docs\技术方案-v0.1.md`，插件格式文档 `plugins/README.md`，项目首页与截图说明在 `README.md`。

## 2. 仓库结构

```
E:\cctest\wb\
  Cargo.toml            # workspace
  build.sh              # 本地 GNU 工具链环境（Git Bash 下 source）
  scripts\build-release.ps1  # release 编译、便携包、哈希和解压冒烟
  .github\workflows\release.yml  # tag / 手动发布流水线
  README.md             # GitHub 项目首页、能力说明与截图
  AGENT_INTEGRATION.md  # CLI / Claude / Cursor / Codex MCP 接入
  LICENSE               # GPL-3.0-only
  assets\panel-ui\index.html   # 面板全部前端（单文件，~2900 行）
  crates\
    wb-core\            # 模型/存储(sqlite)/搜索/协议/命令注册表commands.rs/ai.rs(ask_sync)
    wb-daemon\          # 常驻 JSON-RPC（命名管道 wb-daemon）；clipboard 监听；panelctl；插件装配
    wb-cli\             # wb.exe（注意：bin 名是 wb，不是 wb-cli）
    wb-hook\            # Win 键低级钩子（wb-hook-poc.exe）
    wb-panel\           # 面板宿主：host.rs/ipc.rs/ai.rs(流式+function calling)/dwm.rs(磨砂)/webview2.rs
    wb-plugin-sdk\      # 插件 manifest 类型+权限校验
    wb-plugin-host\     # 插件发现/路径约束/有界进程执行/挂件读取
    wb-mcp\             # stdio MCP server（tools + Skill resources）
  plugins\              # 开发态/发布包内置插件：hello-assistant、clip-insight、stopwatch
  docs-assets\          # 验证截图
  target\{debug,release}\  # Rust 构建产物
  dist\                 # 本地发布产物，已 gitignore
```

## 3. 操作纪律（每条都踩过坑）

1. **编译**：Git Bash 里 `cd /e/cctest/wb && source build.sh && cargo build`。本机当前 rustc 1.98.0 GNU，workspace 最低版本和 CI 为 1.85，链接器在仓库 `.toolchain/mingw64`。
2. 正在运行的同 profile exe 会导致链接 `Permission denied`。优先构建未占用的 profile；只有需要替换运行实例时才集中停进程、构建并恢复，不要为了普通检查反复打断用户。
3. **启动面板必须带 `--wv2`**，否则建的是无 WebView2 的透明空窗。
4. **测试启动面板用 PowerShell Start-Process 且必须带 `-RedirectStandardOutput`**：
   - 不带重定向会弹出黑色控制台窗口（console 子系统），它会被 veil 磨砂采样成"大黑块"——已因此误判过一次 bug。
   - Bash 工具调用结束会收割 `&` 后台子进程；Start-Process 起的进程能存活。所有「启动+等待+截图」要在一个 Bash 调用内完成。
5. **daemon 不用手动管**：任意 `wb` 命令会自动拉起；但改了 daemon 代码要先 taskkill 再用新二进制。
6. **PowerShell 插件脚本必须存成 UTF-8 with BOM**（PS 5.1 无 BOM 按 GBK 解析会炸字符串）；宿主已强制控制台 UTF-8 读写，插件作者不用管管道编码。
7. 全路径：`/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe`。截图脚本 `/tmp/shot2.ps1 -name xxx`（DPI 感知 2560x1440 → `docs-assets/xxx.png`，回复里用 `![x](E:\cctest\wb\docs-assets\xxx.png)` 给用户看）。探针 `/tmp/probe2.ps1`（面板窗口可见性）。
8. 用户常在用机器（打游戏/浏览），**测试面板一律带 `--no-autohide`** 否则失焦自动藏。
9. **页面测试钩子**（hash）：`#test-ai`（纯问答）/ `#test-ai2`（工具调用：加待办）/ `#test-ai3`（插件工具 util_hello）/ `#test-cmd`（> 命令模式）/ `#view=apps` / `#view=plugins` / `#view=market` / `#view=market-sources` / `#dbg-rects` / `#dbg-top`（矩形转储到 stdout）。AI 结果经 selftest 回显宿主 stdout：`grep '"detail":"ai.done' target/xxx.log`。
10. **改 index.html 不用编译**；改完跑语法检查：
    `node -e "const s=require('fs').readFileSync('assets/panel-ui/index.html','utf8'); const m=s.match(/<script>([\s\S]*)<\/script>/); new Function(m[1]); console.log('ok')"`
    注意：内联 `<script>` 里写字符串形式的 `</script>` 必须用 `<\/script>` 或拼接（PLUGIN_SHIM 里有范例）。
11. **AI 配置**：`E:\cctest\api.json` 有中转站信息；模型用 **gpt-5.6-luna**（api.json 里写的别的型号是别的工具的配置，以用户说的为准）。网络问题走代理 `127.0.0.1:7890`（代码已内置网络错误自动代理重试一次）。中转站支持 Responses API + tools/function calling（已实测）。
12. 测试产生的待办/笔记数据测完随手清掉（`wb todo rm`）。
13. 用户正常使用的实例：wb-daemon.exe（托盘/事实源）+ wb-hook-poc.exe（`--panel`，他靠它按 Win）+ 正式 wb-panel（`--wv2`，无测试旗标），各 1 个。hook 与 panel 都有 named mutex 单实例保护；只有基准或多窗口诊断才给 panel 传 `--allow-multiple`。测试完务必恢复正式状态，taskkill 面板不会动 daemon/hook。
14. 回复用户用中文、简洁；结尾一条**加粗**的下一步建议。用户审美要求高——UI 改动要截图自审，对标 DeskBox（参考项目在 `E:\cctest\deskbox-ref`）。
15. 默认只做源码修改、编译、单测、脚本和日志检查。允许必要的前台验收，但要减少频率，把多个视觉/交互检查合并到一次，并提前告诉用户。

## 4. 架构要点（新人 5 分钟版）

- **单一事实源 = wb-daemon**（JSON-RPC over 命名管道）。面板/CLI/MCP 都是平等客户端。方法表见 `wb-core/src/protocol.rs` schema()。
- **命令注册表**（`wb-core/src/commands.rs`）：10 条内建命令。同一份数据三处用：`list_json()`（页面 `>` 模式 + CLI）、`tools_json()`（AI function calling）、方法名直映射（cmd.run 转发）。破坏性命令（clip.clear/system.lock）不暴露给 AI。
- **插件**（M4/M5）：daemon 按用户目录 `%LOCALAPPDATA%/WB/plugins`、exe 同目录 `plugins/`、仓库开发态 `plugins/` 的顺序发现并按 id 去重，用户安装版本优先。带权限插件默认不可见、不可执行，`plugin.approve/revoke` 的授权绑定版本、权限集合和 SHA-256 内容指纹。命令 handler 进程从启动起并发排空 stdout/stderr（每路 1MB、10s 超时），所有插件文件 canonical path 必须留在根目录。工具执行统一走 `cmd.tool.run` 的真实注册表。挂件的 `wbRpc` 经 `plugin.rpc` 身份、批准、权限、白名单四层检查，iframe 默认 CSP 禁止外联。
- **面板视觉**：v11 整屏 veil 磨砂（dwm.rs）：24 张亚克力窗的池，只用第 1 张拉满工作区钉在面板下，显隐各一次定位+淡入淡出，动画期零窗口操作（165Hz 下逐卡跟踪会卡，别回去；WB_CARD_FROST=1 可 A/B）。显隐动画协议：页面先冻结在第 0 帧（hold）→ 宿主亮窗发 "go" → 动画起点即亮窗帧（消灭"先闪一下"）。主看板与桌面常驻组件均可拖动右下角按网格独立调整宽高，panel/desktop 尺寸分别持久化，双击手柄恢复默认；所有内置组件按实际宽高切换 compact/tiny/narrow，插件 iframe 收到 `wbresize {width,height,locale}` 并支持 container query。天气/日历按卡片实际尺寸切换密度，月份位于标题栏、日期严格七行等分，极矮窗口改为看板滚动而不裁内容。AI 输入框有低亮度光路与 `?` 模式增强呼吸光效。
- **AI 链路**：面板 `?` → ai.rs 起线程 curl SSE 流式（系统 curl.exe 零依赖，body 走 stdin）；function calling 回合制最多 3 轮、末轮不带 tools 强制纯文本收尾；daemon 侧 `agent.ask` 走 `wb-core::ai::ask_sync`（非流式）。

## 5. 当前状态：M6 Preview 发布链路已接通

- M1 内核 / M2 面板 / v11 磨砂 / M3 随手问流式 / M3.5 命令注册表+function calling / M4 插件系统。
- M4 实测：`wb cmd run util.hello --arg name=WB` 中文无乱码；`?跟 Luna 打个招呼` → 模型自动调插件 `util_hello` → 确认；sandboxed widget + wbRpc 权限桥正常。
- M5 第一阶段：插件 manifest 支持 `skills` 文档；daemon 暴露 `skill.list` / `skill.get`、`plugin.install` / `plugin.remove`，CLI 提供 `wb skill ...`、`wb plugin pack/install/remove`；ZIP/目录安装会校验、复制到用户目录并立即刷新 daemon 插件池；面板 AI 增加 `skill_list` / `skill_get` 工具。插件页每 3 秒检查清单 revision，新增/删除/替换挂件会自动重建 iframe，另有手动刷新按钮。已批准 widget 进入主看板，插件页只保留管理卡片；仓库内 `plugins/stopwatch` 是首个无权限官方 dogfood 组件。`hello-assistant` 已带 `SKILL.md` 示例。真实 CLI 安装/执行/卸载冒烟、主看板 widget 和插件管理页截图验证通过，workspace 测试全绿。
- M5 Agent 层：`wb-mcp.exe` 已从 stub 升级为 stdio MCP server，支持 tools、resources、prompts 和 resource subscriptions；内建/插件命令从 daemon `cmd.tools` 映射，Skill 保留 `wb://skill/<plugin>/<id>` resource，并同时映射为 `<plugin>__<skill>` MCP prompt。`wb://events/recent` 返回最近 50 条脱敏审计事件，订阅会话在新事件到达时收到 `notifications/resources/updated`。工具携带标准 MCP annotations（只读/破坏性/幂等/开放世界），旧插件缺省按最保守风险；初始化声明 tool/resource/prompt `listChanged`，插件目录变化主动通知客户端重新枚举。真实 stdio 冒烟已验证 `search`、`todo_add`、`skill_get`、临时批准插件 `util_hello` 的标注，以及同一会话安装/批准/卸载时的动态目录通知；daemon 离线时 MCP 会从同目录冷启动并等待最多 5 秒。
- MCP 一键接入：`wb mcp install/status/uninstall claude|cursor|codex` 会结构化合并各客户端默认配置，`generic` 或非默认位置通过 `--file`；JSON 保留其他根设置与 server，Codex TOML 通过 `toml_edit` 保留其他表和注释。写入使用同目录临时文件、flush 后原子替换；同名 `wb` 默认拒绝覆盖/误删，显式 `--force` 才处理冲突条目。`config` 仍保留为只读片段生成器。
- M5 权限与外部接入：manifest 权限白名单、批准/撤销、内容指纹失效、widget RPC 网关、路径与 handler 输出边界已接入。`wb mcp install/status/uninstall` 管理客户端配置，`wb mcp config claude|cursor|codex|generic` 生成只读配置片段，说明见 `AGENT_INTEGRATION.md`。未批准/批准/撤销的 CLI、MCP 与 widget RPC 链路均已真实验证；插件管理页两种状态截图在 `docs-assets/m5-plugin-permissions-*.png`。
- M5 开发者工具：`wb plugin create <id> --kind command|widget|hybrid` 生成包含 Skill 的 Agent-ready 骨架且拒绝覆盖；`wb plugin validate <dir>` 与 `pack` 共用宿主完整性校验，缺 handler/widget/Skill、路径逃逸、大小或 UTF-8 不合规都会失败。`create -> validate -> pack -> install -> approve -> cmd.run -> remove` 真实闭环已通过，生成的 PowerShell handler 返回 `Hello, WB!`，卸载后授权记录清空。
- M5 入口设置：`settings.get` / `settings.set` / `hook.status` 已接入 daemon，面板 ⚙ 弹层和 CLI `wb settings get|win|autostart` 可控制 Win 键接管及 HKCU Run 开机自启；HKCU Run 指向 daemon，由它恢复托盘并按设置启动 hook，hook 通过 `Local\WBHookSingleInstance` 保证单实例。真实测试已验证关闭/开启接管、注册表创建/删除。
- 桌面常驻组件：设置页可选择内置或插件组件常驻桌面，AI 问答也已成为独立组件；独立 `wb-panel.exe --desktop` 使用窗口类 `WBDesktopWidgets` 和单实例 mutex，窗口区域裁剪为实际卡片并保持普通桌面层级。Win 面板默认打开应用页，左侧仍保留完整组件页。CLI 支持 `wb settings desktop <ids...>`，空列表关闭宿主。设置变更通过 `WM_WB_DESKTOP_REFRESH` 事件推送，不再轮询；hook 状态改读 `Local\WBHookSingleInstance`，完全移除会闪控制台的 `tasklist` 查询。
- 多语言：设置页支持 `auto / zh-CN / en / ja / ko`，切换后静态标题、动态搜索/命令/AI/插件市场文案、日期、天气和桌面宿主即时刷新并持久化；CLI 支持 `wb settings language auto|zh-CN|en|ja|ko`。四套翻译表均为 192 个键且无缺项，天气按 WMO code 前端翻译。官方插件示例也会根据宿主传入的 locale 更新文案。
- Spotlight 搜索：应用索引在 daemon 启动阶段同步建立，开始处理命名管道请求前已完成，之后每 5 分钟后台刷新；`apps.list` 和统一搜索只读内存快照，首次打开面板/搜索不会现场扫描或启动 `Get-StartApps`。`wb daemon status` 暴露 `apps_indexed` / `apps_index_ready`。文件类请求优先通过 Everything 1.4/1.5 Unicode v1 IPC 做全盘查询，单次最多 200 条，发送/回包各 1.5 秒超时并严格校验回包；Everything 不可用、数据库未就绪或 IPC 失败时自动降级到 Desktop/Documents/Downloads/OneDrive 的后台有界索引（最多 50,000 项）。结果继续与应用、剪贴板、笔记、待办、插件命令合并排序；状态接口同时暴露 Everything 进程/数据库两个状态。结果 UI 现显示人类可读的来源徽标、选中项详情和真实文件图标；文件支持打开、资源管理器定位、复制路径，笔记/剪贴板支持复制正文，待办可直接完成，`Ctrl+K` / `→` 进入动作区。选择频率与最近使用时间只写本机 `localStorage` 并参与同类结果排序。搜索协议的可选 `preview` 最多 4000 字符，临时隔离配置 E2E 已验证笔记预览和 `wb://todo/<id>` 动作标识。插件结果使用 `wb://cmd/<id>`，面板点击/回车统一进入 `cmd.run`；`#q=` 深链改为在 show/go 握手后消费。视觉回归可用完全脱敏的 `#test-search` 固定数据入口。
- 面板单实例：正常启动使用 `Local\WBPanelSingleInstance` mutex，次实例向 `WBPanelPoc` 发送 `WM_WB_SHOW` 后退出；8 路并发启动实测最终仅 1 个进程，稳定态重复启动日志为 `{"event":"already_running","awakened":true}`。`--bench` 和显式 `--allow-multiple` 绕过该限制供自动化诊断。
- 托盘与退出：daemon 创建 `WBTrayWindow` 通知区入口，左键打开面板，右键菜单提供“打开 WB / 退出 WB”；`wb daemon start|status|stop` 已闭环。status/stop 离线时不隐式启动，stop 在 RPC 响应 flush 后退出，并停止 hook、向 panel 发 `WM_CLOSE`、显式移除托盘图标。CLI stop、重复 stop、托盘退出均真实验证三进程归零。
- 社区远程分发：`plugin.install` 支持 HTTP(S) URL 且强制 SHA-256；`wb plugin pack` 直接返回 `sha256:<hex>`。远程归档限 32MB，解压树限 64MB / 512 文件 / 16 层 / 单文件 16MB；`zip` 解析器在写盘前/写盘中拒绝越界路径、特殊文件、ADS、设备名、大小写冲突和超限内容。下载、解压、staging、backup 均与正式发现目录隔离，安装/卸载事务串行，daemon 重启清理遗留临时工作区，升级提交失败会回滚。极端回滚失败的旧版本保留在 `%LOCALAPPDATA%\WB\plugin-backups\`，只有正式目标存在时才自动清理。真实本机 HTTP E2E 已验证错误哈希拒绝、正确安装/执行/卸载，以及 0.1.0→0.2.0 升级后授权失效和重新批准。
- 开放插件市场：`plugins/market-index.schema.json` 固化 v1 静态索引契约，官方与社区使用同一格式；设置可持久化最多 8 个市场源，CLI/RPC 不传 `index` 时聚合来源并自动解析插件，重复 id 会要求显式选源。面板插件页是“已安装 / 市场”双视图，支持搜索、来源管理、安装、更新与用户插件卸载。市场版本强制 SemVer，远程索引下载限 2MB，远程条目只接受绝对 HTTP(S) 包地址；安装提交前同时核对 SHA-256、插件 id 和版本。真实 HTTP E2E 已验证持久化源、多源聚合和无 `--index` 安装；截图为 `m5-market-page.png`、`m5-market-sources.png`、`m5-plugins-installed-uninstall.png`。
- MCP 服务端策略：设置页和 `wb settings mcp client|ask|read-only` 提供客户端确认、逐次 elicitation、强制只读三档。ask 仅接受 MCP 2025-06-18 且声明 elicitation 的客户端，并要求 `accept + confirm=true`；拒绝、无能力和业务错误统一为 `isError:true`，成功结果带 `structuredContent`。
- 审计与事件：RPC 审计只保留 actor、方法、状态、错误码、耗时和参数形状，旧明文记录启动时一次性脱敏，最多保留 5000 条。`events.tail`、CLI `wb events`、MCP `events_tail` 支持 id 游标和最长 30 秒长轮询。真实 E2E 已验证游标跨连接唤醒，测试 secret 不进入 `wb audit`。
- M6 便携发布：panel 优先从 exe 同目录加载 `assets/panel-ui/index.html` 和 `WebView2Loader.dll`，路径会正确转义空格、`#` 和 UTF-8；daemon 同时发现 exe 同目录内置插件。`scripts/build-release.ps1` 使用 `--release --locked` 构建 5 个 exe，生成固定目录、包内逐文件 `SHA256SUMS.txt`、ZIP 与包外 `.sha256`，并执行版本、解压和关键文件冒烟。GitHub Actions 在手动触发或 `v*` tag 时跑 workspace tests、构建 artifact，并在 tag 上创建 Release。首个本地产物 `RuDock-0.1.0-windows-x64.zip` 已通过 25 个文件的完整哈希复核；`dist/` 不提交。
- 数据可恢复性：`wb backup create [--output PATH]` 使用 SQLite Online Backup 生成一致数据库副本，再打包设置和 `%LOCALAPPDATA%\WB\plugins` 用户插件；归档限制 1024 文件、单文件 16 MiB、总 payload 256 MiB，拒绝符号链接/特殊文件且不覆盖已有输出。结果返回归档 SHA-256，备份单测覆盖在线数据库行保留，真实本机归档冒烟已通过。
- 2026-08-23 全量测试基线：wb-core 12 + wb-daemon 17 + wb-plugin-sdk 12 + wb-plugin-host 6 + wb-cli 12 + wb-mcp 14 + wb-panel 3，共 76 个单测；workspace test 和 release build 通过（wb-panel 仍有原有 11 条 warning）。MCP 配置隔离测试覆盖 JSON/TOML 保留、幂等、冲突、强制替换、原子写入和卸载，真实 CLI `--file` 也完成两种格式的 install→status→uninstall。本机应用索引 341 项、后台文件索引实测 43,927 项；应用列表/搜索热路径无 PowerShell 子进程，Everything 在线真实 IPC 与离线降级、MCP 动态目录通知、read-only 阻断、ask 无能力拒绝、双向 elicitation 接受后真实写入/清理均通过。组件调整大小的 pointer→RPC→settings 持久化链路已在真实 WebView2 宿主冒烟，测试布局随后恢复；日语组件页与英文应用页实机检查无 JS 错误。桌面组件既有无隐私视觉回归截图为 `docs-assets/desktop-widgets.png`。

## 6. 已知瑕疵 / 未验证声明

- Start-Process 不带重定向会出控制台黑窗——生产路径（daemon panelctl 拉起）已用 CREATE_NO_WINDOW，无此问题。
- 插件 widget 支持主看板自动热加载（3 秒轮询 revision）和插件页手动刷新；插件代码仍在 iframe 创建时加载，修改后等待下一轮检查或点刷新。
- `process` 授权仍不是 OS 沙箱：获批 handler 以当前 Windows 用户权限运行。现有权限模型控制 WB 能力暴露和批准生命周期，不隔离任意本地代码。
- daemon 的 `events.tail` 仍是基于审计 id 的长轮询；MCP 已在其上提供标准 resource subscription，对已订阅会话推送资源失效通知，客户端收到后重新读取。每个 MCP stdio 会话使用独立请求连接和目录监听连接，监听线程每 3 秒兜底复核开发态插件变化。
- 便携包已完成静态、哈希、版本和解压冒烟；从完全脱离仓库的解压目录启动 daemon/panel 的前台检查仍应并入最终集中验收。
- MCP elicitation 已落地，但只覆盖经 `wb-mcp.exe` 进入的工具调用；CLI、面板 AI 和 widget 仍遵循各自既有权限边界。
- Everything IPC 已接入，但全盘覆盖取决于用户的 Everything 索引配置和数据库状态；未运行/未就绪时只覆盖 WB 的用户常用目录降级索引。
- 市场底层、持久化多源和可视化页面均已接通；正式官方索引地址尚未配置，必须等真实托管地址，不写占位 URL。

## 7. 建议的下一步（按用户愿景排序）

1. **发行与引导**：在最终集中前台验收后创建首个 `v0.1.0` tag，验证 GitHub Actions 的真实 Release；下一步补安装器、升级检查、首次启动引导、备份恢复和诊断导出。
2. **Agent 生态深化**：一键安装和 MCP 动态 tools/resources/prompts、写策略、elicitation、脱敏事件订阅均已接通；下一步做 Agent profile 与更细粒度的事件过滤。
3. **插件生态深化**：继续把天气、倒数日等内置组件迁移成可独立发布的正式插件；正式官方索引等真实托管地址确定后再配置。
4. **搜索体验深化**：可继续做图片/文本文件内容预览、结果分组折叠与钉到看板。

## 8. 上次会话最后在做的事

完成便携运行布局、PowerShell 发布脚本、包内/包外 SHA-256、GPLv3 许可证、GitHub Release workflow 和 `wb backup create` 数据备份。面向用户的 README 已改为下载、启动、校验、升级、备份和卸载说明。相关提交：`79c13d9`、`d67e15e`、`a5b9b4d`、`7f100fc`、`92cf822`。下一步先补备份恢复/诊断导出，再把便携目录启动、多语言、组件响应式和关键交互合并成一次前台验收。用户允许必要的前台验收，但明确要求尽量减少频率。
