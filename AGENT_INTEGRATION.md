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

- `initialize`, `ping`
- `tools/list`, `tools/call`
- `resources/list`, `resources/read`

内建命令和已批准插件的 AI 命令会成为 MCP tools。插件 command id 会通过真实工具注册表解析，避免用字符串替换猜回 id。已批准的 Skill 以 `wb://skill/<plugin>/<skill>` resource 暴露，另提供 `skill_list` / `skill_get` 工具。

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
```

stdout 被管道时保持机器可读；错误固定为 `{"error":{"code","message","hint"}}`。退出码为 0 成功、2 无结果/NotFound、3 权限不足、4 参数错误、5 daemon 不可用或未实现。

## 插件授权边界

带权限的插件默认不会出现在 MCP tools/resources 中。用户需先在面板插件页批准，或执行：

```text
wb plugin approve <plugin-id>
wb plugin revoke <plugin-id>
```

授权绑定版本、权限集合和插件文件 SHA-256 指纹；代码或 Skill 内容变化后自动失效。`process` 表示 handler 以当前 Windows 用户权限运行，不是 OS 沙箱。完整权限和 widget RPC 契约见 `plugins/README.md`。
