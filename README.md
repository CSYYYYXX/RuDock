# WB — Agent-Native Windows 桌面入口

> 设计文档：[`../docs/技术方案-v0.1.md`](../docs/技术方案-v0.1.md)
> 人按 Win 键用面板，Agent 走 CLI/MCP 用能力，插件一次开发服务两者。

## 当前状态（M5：插件/Skill/MCP + Spotlight 统一搜索）

| 组件 | 状态 |
| --- | --- |
| wb-core（模型/存储/搜索/协议） | ✅ 含 7 个单元测试 |
| wb-daemon（JSON-RPC over Named Pipe） | ✅ 可用，剪贴板实时监听 + 用户文件后台索引 |
| wb-cli（`wb.exe` 全命令面 + `--json` 契约） | ✅ 可用 |
| wb-hook（**Win 键钩子：接管开关 / 开机自启 / 单实例**） | ✅ 可用 |
| wb-panel（PoC 2 PASS；**M2 真面板已通**：搜索/分组/键盘/前缀路由） | ✅ 可用 |
| wb-mcp（M5 Agent 适配层） | ✅ stdio MCP（tools + Skill resources，可冷启动 daemon） |
| wb-plugin-sdk / wb-plugin-host（M4 插件系统） | ✅ 可用（发现/校验/进程执行/挂件桥） |
| 插件生态（M5 第一阶段） | ✅ 可用（Skill / pack / install / remove / daemon 热重载 / 面板挂件热加载 / AI Skill 工具） |

### PoC 结果（验证于本机 Win11 26200，WebView2 Runtime 151）

- **PoC 1（Win 键钩子）**：SendInput 注入自测 PASS —— 裸 Win 吞掉（key-up 判定）、Win+组合键放行、Win+F24 放行。
- **PoC 2（面板）**：
  - 呼出延迟基准 p95 = **21ms**（目标 <100ms）。
  - DWM Mica（`DWMWA_SYSTEMBACKDROP_TYPE`）+ 圆角 + PerMonitorV2 DPI 正常。
  - WebView2 手写 vtable COM 嵌入成功：透明背景 + file:// 本地页 + 导航事件正常，截图 `docs-assets/poc2p.png`。
  - 踩坑记录见方案文档 §10.1（WebView2.h 的 propget 解析坑等 5 条）。
- **剪贴板实时监听**：daemon 内嵌 message-only 窗口 + `WM_CLIPBOARDUPDATE`，复制即入库（去重、10 万字符上限），E2E 已验证。
- **文件搜索降级**：Everything 未接入时，daemon 后台广度优先索引 Desktop/Documents/Downloads/OneDrive（最多 50,000 个文件），搜索请求不被扫描阻塞；Everything IPC 仍是后续的全盘毫秒级方案。

### M2 面板（已验证链路）

- **形态**：右侧屏幕边缘吸附、全工作区高度、520px 宽、不可拖动（WS_POPUP + topmost + toolwindow），失焦自动隐藏。**无底板**：宿主窗 DWMSBT_NONE，每个小组件是一块独立磨砂玻璃卡（CSS `backdrop-filter: blur(26px) saturate(160%)` 直接模糊壁纸），模块浮在壁纸上。截图：`docs-assets/m4-glass2.png`。
- **系统 API 直联**：音乐卡走 **GSMTC**（曲名/歌手/专辑封面/播放状态 + 上一曲/播放暂停/下一曲，`crates/wb-panel/src/media.rs`）；天气卡走 **open-meteo**（免 key，ip-api 粗定位，10 分钟缓存，`weather.rs`，经系统 curl.exe 零依赖）。
- **宿主↔前端通道**：WebView2 `WebMessage`（手写 vtable：`PostWebMessageAsJson`=接口29、`add_WebMessageReceived`=接口31，索引已从 WebView2.h 严格复核）。页面 → 宿主分发（`crates/wb-panel/src/host.rs`）；IPC 走 worker 线程→daemon→`WM_APP+42` 回 UI 线程回推。自测页 `selftest.html` roundtrip PASS。
- **通用 RPC 透传**：页面 `{"kind":"rpc","method":"todo.list",...}` 可直调 daemon 任意方法——小组件全靠它。
- **Win 键集成**：`wb-hook-poc --panel` 吞裸 Win → `WM_WB_TOGGLE(WM_APP+41)` → 面板 show/hide；未运行则拉起。截图：`m2-panel-visible.png` / `m2-panel-hidden.png`。
- **Win 键设置**：面板 ⚙ 设置、`wb settings get|win true|false|autostart true|false` 共用 `%APPDATA%\WB\settings.json`；daemon 按设置自动启动 hook，开机自启写入 HKCU Run；hook 使用 `Local\WBHookSingleInstance` mutex 防重复实例。
- **10 个小组件**（`assets/panel-ui/index.html`）：时钟 / 日历 / 天气(占位) / 系统状态(CPU+内存,宿主 `sysinfo` 每 2s 推) / 番茄钟 / 计算 / 待办(增删勾) / 随手记 / 剪贴板(实时,点击复制) / 应用(预览 4 格 + 全量抽屉网格)。
- **真应用图标**：宿主 `SHGetFileInfoW` 提取 32×32 → 手写零依赖 PNG 编码器 → base64 dataUrl 推给页面缓存（`crates/wb-panel/src/icons.rs`）。无头验证：`wb-panel --icon-test <lnk> <out.png>`。
- **搜索模式**：输入即统一搜索应用、用户文件、剪贴板、笔记、待办和插件命令（分组+真图标+↑↓/Enter）；插件结果回车后走同一个 `cmd.run` 执行面。清空回到组件面板；前缀 `= 计算`（本地）· `? AI`（M3）· `> 命令`（M4）。深链 `#q=` / `#view=apps`。插件搜索视觉验收见 `docs-assets/m5-spotlight-plugin-search-fixed.png`。
- 调试辅助：`wb-hook-poc --inject-win`（注入一次裸 Win 驱动 --panel 钩子做自动化）。

### UI v4：全屏 Spotlight 形态（当前线上版本）

- **形态**：全工作区覆盖（WS_POPUP + topmost + toolwindow），**中央 Spotlight 搜索框**（macOS 聚焦式，顶部居中）+ 下方 6 列马赛克网格铺满全屏，**无滚动、13 个组件一屏全显**；点空白处 / Esc / 再按 Win / 失焦均可关闭。
- **"完全透明"底板 = 截图换底**（`crates/wb-panel/src/backdrop.rs`）：呼出前截取后方桌面（全分辨率 GDI BitBlt → miniz_oxide 压缩 PNG → base64 → `{"kind":"bg"}` 推给页面做整屏背景）。空隙 = 锐利桌面像素；卡片 `backdrop-filter: blur(40px)` 模糊这张图 = **真磨砂玻璃**（WebView2 的 backdrop-filter 本身够不到桌面像素，这是绕行方案）。30s 缓存 + 隐藏后 120ms 后台预截图，二次呼出零开销。坑：GetDIBits 32bpp 无有效 alpha，需强制 255，否则整图透明黑。
- **卡片**：`rgba(22,24,38,.68)` 更高不透明度 + 40px 模糊；主文字纯白不透明，层级 95%/75%。
- **动效**：呼出 = Spotlight 下落 (spotIn) + 卡片 `translateY(26px)+scale(.97)` 交错（38ms/张，cubic-bezier(.16,1,.3,1)）；消失 = 反向 150ms 后页面回 `hide.done` 宿主才真隐藏，380ms 兜底定时器防卡死。`prefers-reduced-motion` 全量降级。
- **13 个组件**：时钟 / 天气(open-meteo) / 日历 / 世界时钟(北京·纽约·伦敦) / 音乐(GSMTC) / 系统(CPU+内存+电池,`GetSystemPowerStatus`) / 番茄钟 / 计算 / 剪贴板 / 待办 / 随手记 / 快捷启动(设置/任务管理器/计算器/记事本/终端/截图,ShellExecute URI+系统路径) / 应用(12 格预览 + 全屏抽屉)。
- 验证截图：`docs-assets/v5-final.png`（稳定态）/ `v5-c-mid.png`（进入中间帧）/ `v5-b-hidden.png`（隐藏后桌面无残留）。
- 已知：钩子自动化注入（SendInput）在本机运行游戏/反作弊（TFT）时收不到事件——真机按实体 Win 键不受影响；测试改用 PostMessage 直发 `WM_WB_TOGGLE`。

### v6/v7：时序修复 + 应用索引补全（当前）

- **呼出卡顿修复（三段握手）**：`show_panel()` 只取**缓存**背景（隐藏期间 120ms 后台预截；绝不现截）→ 发 `{"kind":"show"}` → 页面加 `enter`+`hold`（`animation-play-state:paused` 冻结在第 0 帧）→ 回 `show.ready` → 宿主 `reveal_now()` 才 `ShowWindow` + 发 `go`（双 rAF 后摘 `hold`）。亮窗帧 = 动画第 0 帧，无静帧闪现；200ms 兜底定时器（`SHOW_TIMER_ID`）防页面不应答。截图压缩 miniz_oxide level 4。
- **应用索引补全**：`.lnk` 扫描之外并入 `Get-StartApps`（UWP/Store 应用无 .lnk），按小写标题去重——本机 45 → **287 个**；`shell:AppsFolder\<AppID>` 可 ShellExecute 启动、可 SHCreateItemFromParsingName 提取图标。PowerShell 输出强制 UTF8（`[Console]::OutputEncoding`）修中文乱码；`wb apps` 新子命令直出列表。坑：daemon spawn 必须 `Stdio::null`，否则启动日志污染 CLI 的 stdout JSON；wb-daemon 的 `windows` 特性漏 `Win32_Graphics_Gdi`（WNDCLASSW/RegisterClassW 被它门控），之前靠 wb-panel 特性统一掩盖，单独构建即炸。
- **新组件**：最近文件卡（读 `%APPDATA%\Microsoft\Windows\Recent` 按 mtime 排序，daemon `recent.list`）；精致化一轮——卡片 inset 顶部高光、Spotlight focus 光圈、结果选中行 accent 竖条、快捷启动竖向图标按钮、时钟 200 字重、卡头图标字符。网格 R4 = 最近文件 span2 + 应用 span4。
- 验证截图：`docs-assets/v7-settled.png`（主面板）/ `v7-apps.png`（287 应用抽屉）/ `v6-mid015.png`（动画中间帧无静帧）。

### v8：真透明（DWM 区域模糊，弃用截图换底）

- **架构**：不再截屏假装透明。宿主 `DWMSBT_NONE` + `DwmExtendFrameIntoClientArea(-1)`（客户区变玻璃表面，未涂色处透出**活的桌面**——后面窗口动，面板缝隙实时跟着动）；页面动画落定后把每张卡片矩形（DIP）经 `{"kind":"cardrects"}` 报给宿主，宿主按 `GetDpiForWindow` 换算物理像素，`CreateRoundRectRgn` 合并 HRGN → `DwmEnableBlurBehindWindow(DWM_BB_ENABLE|DWM_BB_BLURREGION)`：**只有卡片区域吃 DWM 实时模糊**（`dwm::set_blur_regions`）。应用抽屉打开时上报全屏矩形 → 整窗磨砂。进入动画期间先清空区域（卡片在位移），`go` 后 620ms 落定再开模糊。
- **连锁收益**：show 路径彻底不截图（bg_capture=0），呼出更快；`--fakebg` 保留旧截图路径做 A/B 回退；`WB_BLUR_FULL=1` 可切整窗模糊调试。
- **关键坑**：①页面 `html,body` 自身的 `background: #0b0c12` 会把透明全挡掉（黑屏），必须 transparent；②没有 ExtendFrameIntoClientArea(-1)，未涂色表面是不透明黑，blur-behind 无从生效；③经典 blur-behind 的模糊强度比 CSS 40px 弱，卡片不透明度提到 .84/.88 补偿可读性。
- 验证截图：`docs-assets/v8-final.png`（缝隙锐利活桌面 + 卡片磨砂）/ `v8-apps.png`（抽屉全屏磨砂，341 应用）。

### v9：磨砂池 + 双页程序坞 + 组件插件化（当前）

- **磨砂池（强磨砂真透明的最终形态）**：经典 blur-behind 太弱、整窗亚克力的 accent 渲染又**忽略 SetWindowRgn 区域裁剪**（磨砂溢出到缝隙）——最终方案是 24 个独立无激活小窗口组成的磨砂池，各自 `SetWindowCompositionAttribute` 开真亚克力，`SetWindowPos(..., panel, ...)` 逐一对位到卡片矩形并钉在面板正下方（内缩 10px 物理像素，方角藏进卡片圆角）。缝隙完全没有窗口 → 活桌面；卡片下是强磨砂。accent 不可用时回退经典区域 blur-behind。
- **闪现修复**：hide 收尾时把舞台冻结在进入动画第 0 帧（全透明），show 时双 rAF 确认第 0 帧合成完毕再回 `show.ready` 亮窗——亮窗帧绝不可能是残留画面。
- **双页结构**：组件页 ⇄ 应用页（mac 程序坞式），`.pages` 200% 宽 flex + 左右拖动（pointer capture，12% 阈值吸附，拖动后全局吞掉一次 click 防误触）+ 圆点指示 + `.38s cubic-bezier(.22,1,.36,1)` 滑动动画；Esc 从应用页先退回组件页。
- **应用自定义**（localStorage `wb.appcfg.v1`）：编辑模式（抖动动画）→ 点 − 隐藏单个应用（底部"已隐藏"区可恢复）；勾选 ≥2 个可**合并为九宫格文件夹**（3×3 迷你图标，点击展开 sheet，编辑态可改名/解散）；应用页内部上下滚动。
- **组件插件化**：Spotlight 栏 ⚙ 打开定制面板，16 个组件任意勾选显隐（localStorage `wb.hiddenWidgets.v1`），磨砂区域随可见性实时重报。新增组件：秒表（rAF 毫秒级）/ 倒数日（日期选择器，持久化）/ 一言（hitokoto API，5s 超时 + 本地兜底句库）。
- 卡片内边距全面加大（hd 15/18px、bd 18px）。
- 验证截图：`docs-assets/v9-frost.png`（磨砂池特写）/ `v9-widgets.png`（组件页）/ `v9-dock.png`（程序坞）。

### v10：两层"合一"——磨砂与卡片同帧 + 淡入淡出（当前）

- **背景**：v9 磨砂池在**落定后**是完美的，但进入动画期间矩形只在动画结束上报一次 → 用户实测"卡片在飞、磨砂钉在原地、落定才刷新"，看起来像两层皮。
- **用户要求单层**（大背景纯透明、磨砂就是组件自己的背景）：实测**单层真模糊在 Win11 上行不通**——本机 25H2/26200 的 `DwmEnableBlurBehindWindow` 经典模糊已被砍到几乎为零（区域/整窗实测都无模糊，见 `v10-single.png`）；亚克力又不吃区域裁剪（v8 已证）。所以磨砂物理上仍须由卡片下方的隐形亚克力窗承担，关键是让它**在视觉上是卡片的一部分**：
  1. **同帧运动**：页面在进入动画 / 切页滑动 / 拖动期间用 `streamRects()` 每个 rAF 上报卡片实时矩形（`getBoundingClientRect` 读变换中的位置），宿主逐帧 `SetWindowPos` 对位——实测一次进入动画上报 128 帧（165Hz 满帧率跟随）。
  2. **淡入淡出**：磨砂窗首次对位时从 alpha 0 淡入（~130ms），隐藏时原位淡出；不会再"先蹦出一块磨砂"或原地残留。
  3. **全强度恢复**：`WS_EX_LAYERED`/`LWA_ALPHA` 路径会**永久削弱亚克力模糊强度**（实测同场景文字可读 vs v9 全糊），所以淡入完成（alpha=255）后立即摘掉 layered 皮（`SetWindowLongPtr(GWL_EXSTYLE)` + `SWP_FRAMECHANGED`），落定态 = 纯亚克力全强度；淡出前再临时披回。
- **日志自证**：页面在 show/go/hide 各阶段回 `selftest` 时戳，宿主落 stdout——`show.ready +15ms / go +42ms / hold removed +52ms / 128 帧流 @165Hz`，时序可审计（`target/wbpanel.out.log`，需重定向启动）。
- **测试基建教训**：`wb-panel.exe` 不带 `--wv2` 参数启动会建一个**纯透明空窗**（无 WebView2），截图验证会全部表现为"窗口可见但全透明"——曾因漏参误诊为"内容没渲染"。探针脚本须确认窗口存在 + WV2 已导航。
- 验证截图：`docs-assets/v10c-mid.png`（动画中帧：磨砂贴着半透的卡片同步推入）/ `v10c-settled.png`（落定：全强度磨砂 + 缝隙锐利活桌面）。
- A/B 开关：`WB_FROST_POOL=0` 关池退回无模糊纯半透明；`WB_BLUR_FULL=1` 整窗经典模糊调试。

### v11：整屏磨砂 veil——流畅优先（当前）

- **起因**：v10 逐卡同帧跟踪在 165Hz 高刷屏上实测"太卡"——每帧 24 个亚克力窗 SetWindowPos，每次都强制亚克力重采样，DWM 合成队列被动画期的逐帧窗口操作打满。
- **方案（用户拍板）**：底层背景 = 一张**覆盖整个工作区的磨砂玻璃**，不做任何动态效果。实现上 = 磨砂池第 1 张窗拉满面板矩形、钉在面板正下方（`DWMWCP_DONOTROUND` 方角），只在显示/隐藏时各定位一次 + 淡入淡出一次，**动画期间零窗口操作**，DWM 始终合成同一张静态表面。页面卡片仍保留自身的进入/退出动画（纯 WV2 内部合成，便宜）。
- 逐卡上报的 `cardrects` 消息在 veil 模式下宿主侧直接忽略（页面无需改）；亚克力不可用时回退官方系统 backdrop（`DWMSBT_TRANSIENTWINDOW`，同样无逐帧操作）。
- A/B：`WB_CARD_FROST=1` 切回 v10 逐卡跟踪模式。
- 验证截图：`docs-assets/v11-mid.png`（veil 先淡入、卡片随后梯队推入）/ `v11-settled.png`（落定：整屏磨砂 + 卡片浮在磨砂上，任务栏保持锐利）。隐藏后枚举磨砂窗可见数 = 0，无残留。

### M3：随手问 AI（当前）

- **链路**：页面 `?` 前缀进入 AI 模式 → 回车发 `ai.ask` → 宿主起工作线程调中转站 **Responses API**（`gpt-5.6-luna`，`stream:true`）→ SSE 逐 token 解析 `response.output_text.delta` → `post_to_page` 回推 `ai.delta/ai.done/ai.error` → 页面流式渲染（mini-markdown：转义 + `code`/`**b**`/换行，闪烁光标）。
- **零新依赖**：HTTP 复用 weather.rs 的路子——系统 `curl.exe -sN --data-binary @-`（stdin 喂 body 避免中文/引号转义坑，`CREATE_NO_WINDOW` 静默）；SSE 按行读，delta 即收即推。
- **容错**：网络类失败自动走本地代理 `127.0.0.1:7890` 重试一次；`response.failed`/`error`/非 SSE 错误体都会转成页面可见的 ⚠ 提示；流中没看到 `completed` 但已收到 delta 也按完成收尾（兼容不规范中转）。配置可用 `WB_AI_URL/WB_AI_KEY/WB_AI_MODEL` 覆盖。
- **交互**：流式期间输入变化不会清掉回答（id + aiStreaming 双闸门）；Esc 先退 AI 模式再退出面板；单次问答（不多轮）。
- **测试钩子**：`--no-autohide`（失焦不藏，截图慢链路必备）+ 页面 `#test-ai` 自动发一次提问并把结果回显到宿主 stdout（`ai.done len=25 head=我在线…`）。
- 验证截图：`docs-assets/m3-ai3.png`（问题 + 流式完成的回答气泡）。实测中转站直连通，端到端 ~5s 出完整短答。

### M3.5：命令注册表——一套数据三处用（当前）

- **核心思想**：`wb-core/src/commands.rs` 单一注册表（10 条命令：todo.add / note.add / search / clip.get / clip.clear / panel.hide / panel.show / panel.toggle / system.lock / agent.ask），同一份数据产出三种形态——`list_json()` 给页面/CLI 渲染、`tools_json()` 给 Responses API function calling、daemon 方法名直接映射给 CLI。
- **三处消费**：
  1. **面板 `>` 命令模式（人）**：输入 `>` 拉 `cmd.list` 缓存 → id/title/hint 模糊过滤（前缀命中排前）→ 方向键/回车 `cmd.run`；缺参数自动补位提示并回填输入框；`>搜索 xxx` 就地转普通搜索、`>问AI xxx` 就地转 `?` 模式。
  2. **AI 工具调用（模型）**：`wb-panel/src/ai.rs` 回合制 function calling——SSE 流里解析 `response.output_item.done` 中的 `function_call` 项 → 本地执行（panel.hide 直接 `PostMessageW(WM_WB_HIDE)`，其余经 IPC 转 daemon）→ 组装 followup input（user + function_call + function_call_output）开下一回合，最多 3 轮、末轮不带 tools 强制纯文本收尾。search 限 8 条、clip.get 截 200 字防爆上下文。破坏性命令（clip.clear / system.lock）故意不暴露给模型。
  3. **wb CLI（外部 Agent）**：`wb cmd list` / `wb cmd run <id> --arg k=v`，原样转发 `cmd.list`/`cmd.run`；`wb agent ask "…"` 走 daemon 侧 `wb-core::ai::ask_sync`（非流式，`stream:false`），`wb panel show/hide/toggle` 经 `panelctl.rs` FindWindow + PostMessage 跨进程控制（面板没在跑时自动以 `--wv2` 拉起）。
- **daemon 补齐**：`cmd.list` / `cmd.run`（注册表 id 原样当 daemon 方法转发）/ `panel.show|hide|toggle` / `agent.ask` / `audit.tail`；`system.lock` 用 `rundll32 user32.dll,LockWorkStation`。
- **实测**（2026-08-22）：`wb cmd run todo.add --arg title=…` 落库并在面板待办组件实时可见；`wb agent ask` 返回 `gpt-5.6-luna` 应答；CLI `panel show/hide/toggle` 跨进程显隐全部幂等生效；AI 实测 `?帮我加一条待办：明天下午三点开会` → 模型自主调 `todo_add`（title=开会，due 自动解析"明天下午三点"）→ 落库 → 文本确认，甚至顺手调了 `panel_hide` 收面板（并行工具调用也验证到了）。
- 截图：`docs-assets/m35-ai-toolcall.png`（AI 气泡含 ⏳ 工具执行行 + 待办组件实时更新）、`docs-assets/m35-cmdmode.png`（`>todo` 命令模式渲染）。
- 测试钩子：`#test-ai2`（自动发工具调用提问）、`#test-cmd`（自动打开 `>todo` 命令模式）。

### M4：插件系统——小组件与 Agent 能力统一开放（当前）

- **格式**：一个文件夹 + `plugin.json`（`wb-plugin-sdk` 定义 manifest 与校验，5 个单测）。两个示例在 `plugins/`：`hello-assistant`（命令插件，PowerShell handler）与 `clip-insight`（挂件插件）。完整格式文档：`plugins/README.md`。
- **命令插件**：manifest 声明 `commands`（id/title/hint/arg/ai 描述），handler 子进程契约——stdin 收 `{"command", "args"}`、stdout 吐 JSON、10s 超时强杀；`.ps1/.js/.py/.exe` 按扩展名映射解释器（ps1 强制控制台 UTF-8 + 脚本需带 BOM）。**声明一次三处可用**：面板 `>` 模式、`wb cmd run`、AI function calling（daemon `cmd.tools` 合并注册表内建 + 插件 ai 命令；工具名点换下划线，执行时还原，面板 AI 一律走 `cmd.run` 不再分辨内建/插件）。
- **挂件插件**：manifest 声明 `widget`（单文件 HTML）→ 面板新增**第三页「插件」页**（三页拖动/圆点/滑动动画全套），挂件以 sandboxed iframe（`allow-scripts` + srcdoc）装进玻璃卡；内置 `wbRpc(method, params)` 桥——iframe postMessage → 父页中继 → daemon JSON-RPC，插件组件可直接调 daemon 能力（clip-insight 演示读剪贴板统计）。插件卡自动注册进组件定制（⚙ 可隐藏）。
- **daemon 新方法**：`plugin.list` / `plugin.run` / `plugin.reload` / `plugin.install` / `plugin.remove` / `plugin.widget` / `cmd.tools` / `skill.list` / `skill.get`；插件目录 = `%LOCALAPPDATA%/WB/plugins`（用户）+ 仓库 `plugins/`（开发，exe 上三级自动发现）；安装会校验并复制目录或 ZIP，随后立即刷新插件池。插件列表带 widget revision，面板插件页据此自动热加载。
- **实测**（2026-08-22）：`wb cmd run util.hello --arg name=WB` 中文往返无乱码；AI 实测 `?跟 Luna 打个招呼` → 模型自主调 `util_hello` → PS1 插件执行 → 自然语言确认；插件页挂件正常渲染。截图：`docs-assets/m4-ai-plugin.png`、`docs-assets/m4-plugins-page.png`。
- **边界**：插件是用户自装的本地代码，v1 权限仅声明不强制；破坏性命令不暴露 `ai` 段即可避开模型。
- **Skill**：插件可以随附 Markdown Skill 文档；面板 AI 通过 `skill_list` / `skill_get` 读取工作流说明，再调用插件命令完成任务。Skill 不拥有额外执行权限。
- **MCP**：`wb-mcp.exe` 通过 stdio 提供 MCP `initialize` / `tools/list` / `tools/call` / `resources/list` / `resources/read`；工具转发 daemon `cmd.run`，Skill 以 `wb://skill/<plugin>/<id>` resource 暴露。daemon 离线时 MCP 会从同一产物目录静默拉起它并等待就绪，Claude/Cursor 等 Agent 不需要了解 Windows Named Pipe 或预先管理进程。

## 构建环境（Windows，已固化在本仓库）

Rust 工具链：用户目录 rustup（`stable-x86_64-pc-windows-gnu`）。
C 链接工具链：仓库内 `.toolchain/mingw64`（winlibs UCRT64，随取随用，不入库）。

```bash
# Git Bash 下：
source build.sh        # 配好 PATH 等环境变量
cargo build            # 构建全部 crate
cargo test -p wb-core  # 跑核心单测
```

## 快速验证

```bash
# PoC 1：Win 键钩子自检（注入按键验证 吞裸Win/放组合键 判定）
./target/debug/wb-hook-poc.exe --self-test

# 端到端（一个终端起 daemon，另一个用 CLI）：
./target/debug/wb-daemon.exe &
wb note add "hello" --tag demo --json
wb todo add "发周报" --due friday --json
wb clip add "https://example.com" --json
wb search "周报" --json          # 命中 → exit 0
wb search "不存在xyz"            # 无结果 → exit 2
wb panel show                    # 跨进程显示正式 WebView2 面板
wb schema --json                 # Agent 自省命令面
```

## CLI 输出契约（§5.2）

- 任何命令 `--json`；`search` 另支持 `--ndjson` 逐行流
- stdout 被管道时自动切纯 JSON（无颜色无装饰）
- exit code：0 成功 / 2 无结果或 NotFound / 3 权限不足 / 4 参数错误 / 5 daemon 不可用或未实现
- 错误永远结构化：`{"error":{"code":...,"message":...,"hint":...}}`
- `wb schema --json` 自输出完整命令树 + 参数说明

## 已知事项

- AI provider 测试端点（api.json）：`/v1/models` 可列、`gpt-5.6-luna` 在列，但推理端点（responses 与 chat/completions）目前返回 `Service temporarily unavailable`。M3 才用到，不阻塞。代理规则：网络异常走 `http://127.0.0.1:7890`。
- `wb daemon stop` 与 Everything 真 IPC 尚未实现；当前文件检索是用户常用目录的有界后台索引，不等同于全盘实时索引。
