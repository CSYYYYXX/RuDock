# WB Agent / MCP 接入

WB 对外提供两层稳定接口：适合 MCP 客户端的 `wb-mcp.exe`，以及适合 shell/Agent 脚本的 `wb.exe --json`。两者都以 `wb-daemon.exe` 为单一事实源，不需要调用或解析面板 UI。

## MCP 快速配置

先构建 workspace，确保 `wb.exe`、`wb-mcp.exe` 与 `wb-daemon.exe` 位于同一产物目录。随后让 CLI 按当前可执行文件的绝对路径生成配置：

```text
wb mcp config claude
wb mcp config cursor
wb mcp config codex
wb mcp config generic
```

- `claude` / `cursor` / `generic` 输出 `mcpServers` JSON，可合并进客户端的 MCP 配置文件。
- `codex` 输出可合并进 `%USERPROFILE%\.codex\config.toml` 的 TOML：

```toml
[mcp_servers.wb]
command = "E:\\path\\to\\wb-mcp.exe"
args = []
```

使用 `--json` 时，生成结果包装为 `{"client","config"}`，便于安装器或 Agent 自动消费。

## MCP 能力

`wb-mcp.exe` 是 stdio MCP server，支持：

- `initialize`, `ping`（协商支持 `2024-11-05` / `2025-06-18`）
- `tools/list`, `tools/call`
- `resources/list`, `resources/read`, `resources/subscribe`, `resources/unsubscribe`
- `prompts/list`, `prompts/get`
- `notifications/tools/list_changed`, `notifications/resources/list_changed`, `notifications/resources/updated`, `notifications/prompts/list_changed`

内建命令和已批准插件的 AI 命令会成为 MCP tools。插件 command id 会通过真实工具注册表解析，避免用字符串替换猜回 id。已批准的 Skill 以 `wb://skill/<plugin>/<skill>` resource 暴露，并同时成为 `<plugin>__<skill>` MCP prompt；另提供 `skill_list` / `skill_get` 工具。prompt 获取会先查询当前 Skill 目录中的真实映射，插件撤销批准后不能继续读取旧内容。

初始化响应会声明 `tools.listChanged=true`、`resources.listChanged=true`、`resources.subscribe=true` 和 `prompts.listChanged=true`。插件安装、升级、卸载、批准、撤销或开发态刷新真正改变目录后，WB 会主动发送对应 notification；客户端收到后应重新调用 `tools/list`、`resources/list` 或 `prompts/list`，无需重启 MCP 会话。目录通知只表示目录失效，不携带插件内容。

`wb://events/recent` 是固定的只读 JSON resource，读取时返回最近 50 条脱敏审计事件。客户端对它调用 `resources/subscribe` 后，新事件到达会收到：

```json
{"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":"wb://events/recent"}}
```

收到通知后重新调用 `resources/read` 即可；`resources/unsubscribe` 会停止当前 MCP 会话的更新通知。其他 resource URI 不接受订阅。事件只含 actor、动作、状态、错误码、耗时和参数形状，不含笔记、剪贴板、AI prompt 等正文。

每个 tool 都带 MCP 标准 `annotations`：`readOnlyHint`、`destructiveHint`、`idempotentHint`、`openWorldHint`，并附可读标题。搜索、剪贴板读取和 Skill 读取标记为本地只读；新增待办/笔记标记为非破坏性写入；插件命令使用 manifest 声明，旧插件或缺失声明一律按最保守风险处理。

WB 另有独立的 MCP 写操作策略：

```text
wb settings mcp client     # 默认：沿用客户端自己的确认
wb settings mcp ask        # 每次非只读调用要求 elicitation/create
wb settings mcp read-only  # 服务端阻止所有非只读调用
```

`ask` 仅对 MCP 2025-06-18 且 initialize capabilities 声明 `elicitation` 的客户端开放；WB 只在响应为 `action: accept` 且 `content.confirm: true` 时执行。未声明能力、decline、cancel 或未勾确认都会返回 `isError:true`，不会写入数据。成功工具结果提供文本 `content` 和机器可读 `structuredContent`。

需要自行维护游标时，也可以使用只读工具 `events_tail`，参数为 `after`、`limit`、`wait_ms`。它按审计 id 增量返回事件，`wait_ms` 最长 30000。

若 daemon 未运行，MCP 会从 `wb-mcp.exe` 所在目录静默启动 `wb-daemon.exe`，等待就绪后继续当前请求。客户端不需要了解 Windows Named Pipe。

## CLI 接入

不支持 MCP 的 Agent 可直接使用：

```text
wb schema --json
wb cmd list --json
wb cmd run todo.add --arg title="检查发布" --json
wb search "发布" --type note --json
wb skill list --json
wb skill get hello-assistant greeting --json
wb events --after 0 --limit 50 --wait-ms 30000 --json
```

stdout 被管道时保持机器可读；错误固定为 `{"error":{"code","message","hint"}}`。退出码为 0 成功、2 无结果/NotFound、3 权限不足、4 参数错误、5 daemon 不可用或未实现。

## 插件授权边界

带权限的插件默认不会出现在 MCP tools/resources 中。用户需先在面板插件页批准，或执行：

```text
wb plugin approve <plugin-id>
wb plugin revoke <plugin-id>
```

授权绑定版本、权限集合和插件文件 SHA-256 指纹；代码或 Skill 内容变化后自动失效。`process` 表示 handler 以当前 Windows 用户权限运行，不是 OS 沙箱。完整权限和 widget RPC 契约见 `plugins/README.md`。
