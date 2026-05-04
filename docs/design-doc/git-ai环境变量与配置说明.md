# git-ai 环境变量与配置说明

本文说明当前 `git-ai` 常用环境变量、`git-ai config` 配置项、查看方式和设置方式。排查“AI 归因错误”“统计结果未上传”“本机安装来源不对”时，优先看这里。

## 一、配置优先级

`git-ai` 的配置来源大致分三类：

1. 环境变量：当前 shell、用户环境变量、系统环境变量、CI Secret 注入等。
2. `git-ai config` 文件：通常在 `~/.git-ai/config.json`。
3. 代码默认值。

Feature flags 的优先级是：环境变量 `GIT_AI_*` > `~/.git-ai/config.json` 中的 `feature_flags.*` > 默认值。

上传远程地址、上传 API key、上传 user id 当前只读环境变量，不读 `git-ai config`。

## 二、怎么查看当前值

### 1. 查看当前 shell 中的环境变量

PowerShell：

```powershell
$env:GIT_AI_ASYNC_MODE
$env:GIT_AI_DEBUG
$env:GIT_AI_AUTO_UPLOAD_AI_STATS
$env:GIT_AI_REPORT_REMOTE_URL
```

如果输出为空，表示当前 shell 没有这个变量，不代表用户级或系统级一定没有。

### 2. 查看 Windows 用户级 / 系统级环境变量

PowerShell：

```powershell
[Environment]::GetEnvironmentVariable("GIT_AI_ASYNC_MODE", "User")
[Environment]::GetEnvironmentVariable("GIT_AI_ASYNC_MODE", "Machine")
[Environment]::GetEnvironmentVariable("GIT_AI_REPORT_REMOTE_API_KEY", "User")
```

敏感变量不要直接完整打印。API key 建议只看是否存在，最多看前几位：

```powershell
$key = [Environment]::GetEnvironmentVariable("GIT_AI_REPORT_REMOTE_API_KEY", "User")
if ($key) { $key.Substring(0, [Math]::Min(4, $key.Length)) + "***" } else { "未设置" }
```

### 3. 查看 git-ai 的有效配置

```powershell
git-ai config
git-ai config feature_flags
git-ai config feature_flags.async_mode
git-ai config prompt_storage
```

`git-ai config` 输出的是运行时有效配置，`api_key` 会脱敏。注意：环境变量优先级更高，所以 `git-ai config feature_flags.async_mode` 可能已经体现了 `GIT_AI_ASYNC_MODE` 的覆盖结果。

### 4. 查看 debug.jsonl

Windows 默认路径：

```powershell
Get-Content "$env:USERPROFILE\.git-ai\logs\debug.jsonl" -Tail 50
```

Linux / macOS 默认路径：

```bash
tail -n 50 ~/.git-ai/logs/debug.jsonl
```

当前 `debug.jsonl` 默认开启。每条日志都会包含 `timestampMs` 和人类可读的 UTC `timestamp`。当文件超过 2GB 时，`git-ai` 会尽力只保留最近约 512MB 内容后继续写入，避免无限占用用户磁盘。日志写入失败、裁剪失败都不会阻断 commit、stats 或上传主流程。

## 三、怎么设置变量

### 1. 只对当前 PowerShell 窗口生效

```powershell
$env:GIT_AI_ASYNC_MODE = "false"
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "true"
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
```

### 2. Windows 用户级持久生效

设置后需要打开新的终端或重启 VS Code 终端才会读取到。

```powershell
[Environment]::SetEnvironmentVariable("GIT_AI_ASYNC_MODE", "true", "User")
[Environment]::SetEnvironmentVariable("GIT_AI_AUTO_UPLOAD_AI_STATS", "true", "User")
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_API_KEY", "your-api-key", "User")
```

删除用户级变量：

```powershell
[Environment]::SetEnvironmentVariable("GIT_AI_ASYNC_MODE", $null, "User")
```

### 3. Linux / macOS 当前 shell 生效

```bash
export GIT_AI_ASYNC_MODE=true
export GIT_AI_AUTO_UPLOAD_AI_STATS=true
export GIT_AI_REPORT_REMOTE_URL="https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
```

### 4. 通过 git-ai config 设置 feature flag

```powershell
git-ai config set feature_flags.async_mode true
git-ai config set feature_flags.auto_upload_ai_stats true
git-ai config set prompt_storage notes
```

如果同时设置了环境变量和 `git-ai config`，以环境变量为准。

## 四、常用运行时环境变量

| 变量 | 作用 | 默认/规则 | 建议 |
| --- | --- | --- | --- |
| `GIT_AI_ASYNC_MODE` | 控制是否使用 daemon/异步模式处理 git wrapper、checkpoint 等链路 | debug 构建默认 `false`，release 构建默认 `true`；`true/false` 布尔解析 | 正式安装建议保持默认或设为 `true`；本地复现归因问题时可临时设为 `false`，便于同步验证 |
| `GIT_AI_AUTO_UPLOAD_AI_STATS` | 控制 commit 后是否自动上传 AI 统计结果 | 当前默认开启；可用 `true/false` 覆盖 | 正式使用建议保持开启；如果远程服务暂不可用，可临时设为 `false` |
| `GIT_AI_DEBUG` | 控制本地 JSONL 诊断日志 | 当前默认开启；设为 `false`、`0`、`off`、`no` 可关闭 | 建议保持默认开启，排查归因和上传问题时非常关键 |
| `GIT_AI_DEBUG_STDERR` | 控制是否把部分 debug 信息打到 stderr | 默认关闭；debug 构建除外 | 普通用户不需要设置；需要现场看终端细节时再临时开启 |
| `GIT_AI_API_BASE_URL` | Git AI 官方/自托管 API 地址 | 默认 `https://usegitai.com` | 使用官方或自托管 Git AI 后端时配置 |
| `GIT_AI_API_KEY` | Git AI 官方/自托管 API key | 环境变量优先于 config 文件 | 敏感值，只放用户环境变量或 CI Secret，不写进仓库 |
| `GIT_AI_CUSTOM_ATTRIBUTES` | 给 prompt metadata 注入自定义属性 | JSON 对象字符串，环境变量会覆盖同名 config 属性 | 需要把团队、项目、组织等标签随 note/统计带出时使用 |

## 五、Feature flags 对应变量

这些变量来自 `feature_flags`，环境变量名统一是 `GIT_AI_` + 配置名大写。

| 环境变量 | config 键 | 作用 | 默认 |
| --- | --- | --- | --- |
| `GIT_AI_REWRITE_STASH` | `feature_flags.rewrite_stash` | 包装 git stash 等操作时保留/改写归因上下文 | debug/release 都为 `true` |
| `GIT_AI_CHECKPOINT_INTER_COMMIT_MOVE` | `feature_flags.checkpoint_inter_commit_move` | 跨 commit move 归因实验开关 | debug/release 都为 `false` |
| `GIT_AI_AUTH_KEYRING` | `feature_flags.auth_keyring` | 认证信息使用系统 keyring 的实验开关 | debug/release 都为 `false` |
| `GIT_AI_ASYNC_MODE` | `feature_flags.async_mode` | 是否启用异步 daemon 模式 | debug 为 `false`，release 为 `true` |
| `GIT_AI_GIT_HOOKS_ENABLED` | `feature_flags.git_hooks_enabled` | 旧 git hooks 模式开关 | debug/release 都为 `false`；开启时会迁移到 async 模式 |
| `GIT_AI_GIT_HOOKS_EXTERNALLY_MANAGED` | `feature_flags.git_hooks_externally_managed` | 声明 hooks 由外部系统管理 | debug/release 都为 `false` |
| `GIT_AI_FORMAT_PASSTHROUGH` | `feature_flags.format_passthrough` | 纯格式化/空白改动是否透传原有归因 | debug/release 都为 `true` |
| `GIT_AI_AUTO_UPLOAD_AI_STATS` | `feature_flags.auto_upload_ai_stats` | commit 后是否自动上传 AI 统计 | debug/release 都为 `true` |

布尔值建议只写 `true` 或 `false`，不要依赖 `1/0`。

## 六、统计上传相关变量

| 变量 | 作用 | 规则 |
| --- | --- | --- |
| `GIT_AI_REPORT_REMOTE_URL` | 完整上传 URL，优先级最高 | 设置后忽略 endpoint/path 组合 |
| `GIT_AI_REPORT_REMOTE_ENDPOINT` | 上传服务 host/base URL | 必须和 `GIT_AI_REPORT_REMOTE_PATH` 同时设置才生效 |
| `GIT_AI_REPORT_REMOTE_PATH` | 上传路径 | 和 endpoint 拼成完整 URL |
| `GIT_AI_REPORT_REMOTE_API_KEY` | 上传接口 Bearer token | 可选；存在时写入 `Authorization: Bearer ...` |
| `GIT_AI_REPORT_REMOTE_USER_ID` | 上传接口 `X-USER-ID` | 优先使用；未设置时会尝试从 VS Code / IDEA MCP 配置读取 |
| `GIT_AI_VSCODE_MCP_CONFIG_PATH` | 覆盖 VS Code MCP 配置文件路径 | 用于从指定 `mcp.json` 读取 `X-USER-ID` |
| `GIT_AI_IDEA_MCP_CONFIG_PATH` | 覆盖 IDEA MCP 配置文件路径 | 用于从指定 `mcp.json` 读取 `X-USER-ID` |

上传 URL 解析顺序：

1. `GIT_AI_REPORT_REMOTE_URL`
2. `GIT_AI_REPORT_REMOTE_ENDPOINT` + `GIT_AI_REPORT_REMOTE_PATH`
3. 内置默认地址 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`

如果上传失败，看 `debug.jsonl` 中这些事件：`post_commit_upload_dispatch_requested`、`upload_stats_auto_entered`、`upload_stats_payload_build_started`、`upload_stats_payload_build_succeeded`、`upload_stats_ready`、`upload_stats_started`、`upload_stats_http_request_ready`、`upload_stats_http_response_received`、`upload_stats_http_non_success`、`upload_stats_failed`。

## 七、安装脚本相关变量

这些变量只影响 `install.ps1` / `install.sh`，不影响已安装后的日常归因逻辑。

| 变量 | 作用 |
| --- | --- |
| `GIT_AI_GITHUB_REPO` | 覆盖 release 来源仓库，例如 `rj-gaoang/git-ai` |
| `GIT_AI_RELEASE_TAG` | 覆盖 release tag，例如 `latest` 或固定版本 tag |
| `GIT_AI_LOCAL_BINARY` | 跳过下载，直接把本地 binary 复制到安装目录 |
| `GIT_AI_SKIP_PATH_UPDATE` | Windows 安装时设为 `1` 可跳过 PATH 更新 |
| `GIT_AI_INSTALLER_URL` | 供上层集成脚本选择 installer 下载地址；当前安装脚本本身不直接消费这个变量 |

## 八、内部 / 调试 / 测试变量

这些变量一般不建议普通用户手动设置，除非在复现 bug 或跑测试。

| 变量 | 作用 |
| --- | --- |
| `GIT_AI_DAEMON_HOME` | 覆盖 daemon 的 home/internal 目录，测试或隔离 daemon 时使用 |
| `GIT_AI_DAEMON_CONTROL_SOCKET` | 覆盖 daemon 控制 socket 路径 |
| `GIT_AI_DAEMON_CHECKPOINT_DELEGATE` | daemon checkpoint 子进程委派标记，内部使用 |
| `GIT_AI_SKIP_ALL_HOOKS` | 内部子命令防止递归触发 hooks，内部自动设置 |
| `GIT_AI_POST_COMMIT_TIMEOUT_MS` | post-commit 等待超时时间，主要给测试覆盖 |
| `GIT_AI_TEST_FORCE_TTY` | 测试时强制认为 stdout 是 TTY |
| `GIT_AI_WRAPPER_INVOCATION_ID` | git wrapper 透传给 Git Trace2 的调用 ID，内部自动设置 |
| `GIT_AI_DEBUG_PERFORMANCE` | 性能调试输出；大于等于 2 时输出结构化 JSON |
| `GIT_AI_DEBUG_DAEMON_TRACE` | daemon trace normalizer 调试开关 |
| `GIT_AI_CLOUD_AGENT` | 标记云端/远程 agent 环境 |
| `GIT_AI_TEST_CONFIG_PATCH` | 测试时用 JSON patch 覆盖 config |
| `GIT_AI_TEST_DB_PATH` | 测试时覆盖 authorship/prompt 数据库路径 |
| `GIT_AI_TEST_METRICS_DB_PATH` | 测试时覆盖 metrics 数据库路径 |

## 九、常见排查组合

### 归因错误

```powershell
git-ai config feature_flags.async_mode
$env:GIT_AI_ASYNC_MODE
Get-Content "$env:USERPROFILE\.git-ai\logs\debug.jsonl" -Tail 120
```

重点看 checkpoint 事件：`checkpoint_explicit_path_resolution`、`checkpoint_attribution_decision`、`checkpoint_no_entries`、`known_human_checkpoint_rejected`。

### 统计未上传

```powershell
git-ai config feature_flags.auto_upload_ai_stats
$env:GIT_AI_AUTO_UPLOAD_AI_STATS
$env:GIT_AI_REPORT_REMOTE_URL
$env:GIT_AI_REPORT_REMOTE_ENDPOINT
$env:GIT_AI_REPORT_REMOTE_PATH
$env:GIT_AI_REPORT_REMOTE_USER_ID
Get-Content "$env:USERPROFILE\.git-ai\logs\debug.jsonl" -Tail 160
```

重点看 post-commit 和 upload 事件：如果 `post_commit_stats_skipped` 出现，说明本次 commit 没有可上传 stats；如果出现 `upload_stats_http_non_success` 或 `upload_stats_failed`，看 `statusCode` 和错误摘要。

### 安装来源不对

```powershell
Get-Command git-ai
git-ai --version
[Environment]::GetEnvironmentVariable("GIT_AI_GITHUB_REPO", "User")
[Environment]::GetEnvironmentVariable("GIT_AI_RELEASE_TAG", "User")
[Environment]::GetEnvironmentVariable("GIT_AI_LOCAL_BINARY", "User")
```

确认当前命中的 binary 路径、版本，以及安装脚本是否被 release/tag/local binary 覆盖。