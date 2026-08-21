$in = [Console]::In.ReadToEnd() | ConvertFrom-Json
$name = $in.args.name
if ([string]::IsNullOrWhiteSpace($name)) { $name = "朋友" }
@{
  greeting = "你好，$name！我是 WB 插件「Hello 小助手」，很高兴为你服务。"
  command  = $in.command
  at       = (Get-Date).ToString("HH:mm:ss")
} | ConvertTo-Json -Compress
