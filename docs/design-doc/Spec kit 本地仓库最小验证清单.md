# Spec kit 本地仓库最小验证清单（2026-04-18 实测版）

|执行耗时|10~15分钟|
|---|---|

## 适用场景

用于验证本地 `spec-kit-standalone/spec-kit` 源码重新安装到 `specify-cli` 后，Windows 下 `--offline` 初始化链路是否真正使用了本地最新代码，并确认以下问题已经修复：

1. `uv tool install --force` 仍可能复用旧缓存包
2. `specify init --offline` 未走本地 bundled assets
3. `post-init` 在缺少 `pwsh` 的机器上报 `WinError 2`
4. 本地离线打包时出现 `ZipArchiveHelper` / `Compress-Archive` 文件占用错误
5. 主动执行 `upload-ai-stats.ps1` 时未命中最终 AI 统计上传接口
6. `/speckit.code-review` 触发 AI 统计上传时未命中最终 AI 统计上传接口

本文档默认使用以下路径作为示例：

```Plain Text
源码仓库：D:\git-ai-main\spec-kit-standalone\spec-kit
验证项目：D:\rj-op
```

如果你的路径不同，请替换成你自己的目录。

## 本次实测环境

- 操作系统：Windows
- `pwsh`：未安装
- `powershell.exe`：可用
- `git-ai --version`：`1.3.0`
- 本次重装得到的 `specify-cli` 版本：`0.0.79`

## 验证目标

完成本清单后，应至少确认以下结果：

1. `uv` 已从本地源码重新构建并安装当前版本的 `specify-cli`
2. `specify init --here --force --ai copilot --script ps --offline` 能成功完成初始化
3. 初始化输出中 `Run post-init hooks` 状态为 `ok`
4. 不再出现 `Run post-init hooks ([WinError 2] 系统找不到指定的文件。)`
5. `post-init.ps1 -Skip` 能正常执行，`git-ai --version` 可正常返回
6. 离线打包链路不再出现 `ZipArchiveHelper` / `Compress-Archive` 文件占用报错
7. 主动执行 `upload-ai-stats.ps1` 时默认调用 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`
8. 执行 `/speckit.code-review` 时，AI 统计上传也调用同一个最终接口

## 实测步骤

### 1. 确认本地源码位于待验证分支

```PowerShell
cd D:\git-ai-main\spec-kit-standalone\spec-kit
git branch --show-current
git log --oneline -1
```

通过标准：

- 当前分支为你要验证的开发分支
- 最新提交为你本次要验证的代码

### 2. 卸载旧版本并强制从本地源码重装 specify-cli

```PowerShell
cd D:\git-ai-main\spec-kit-standalone\spec-kit
uv tool uninstall specify-cli
uv tool install specify-cli --from "D:\git-ai-main\spec-kit-standalone\spec-kit" --force --no-cache
specify --help
```

通过标准：

- 安装输出包含 `specify-cli==0.0.79 (from file:///D:/git-ai-main/spec-kit-standalone/spec-kit)`
- 终端显示 `Installed 1 executable: specify`
- `specify --help` 能正常输出帮助信息

说明：

- 本地源码调试场景下，`--force` 不足以保证使用最新代码，仍可能命中 `uv` 缓存
- 本次实测使用 `uv tool uninstall` + `--force --no-cache` 后，安装目录中的新逻辑才真正生效

### 3. 在目标项目目录验证离线初始化链路

```PowerShell
cd D:\rj-op
specify init --here --force --ai copilot --script ps --offline
```

通过标准：

- 输出包含 `Apply bundled assets (bundled assets applied)`
- 输出包含 `Run post-init hooks (ok)`
- 输出包含 `Project ready.`
- 不再出现 `Run post-init hooks ([WinError 2] 系统找不到指定的文件。)`
- 不再出现 `Run post-init hooks (script not found)`

### 4. 冒烟验证 post-init 回退逻辑与 git-ai 命令状态

```PowerShell
cd D:\rj-op
.\.specify\scripts\powershell\post-init.ps1 -Skip
git-ai --version
```

通过标准：

- `post-init.ps1 -Skip` 输出跳过信息且不报错
- `git-ai --version` 正常返回版本号

### 5. 验证主动调用 upload-ai-stats.ps1 会命中最终接口

```PowerShell
cd D:\rj-op
$commit = git log --format=%H -1
$env:GIT_AI_REPORT_REMOTE_API_KEY = "your-test-api-key"
$env:GIT_AI_REPORT_REMOTE_USER_ID = "your-user-id"
Remove-Item Env:GIT_AI_REPORT_REMOTE_URL -ErrorAction SilentlyContinue
Remove-Item Env:GIT_AI_REPORT_REMOTE_ENDPOINT -ErrorAction SilentlyContinue
Remove-Item Env:GIT_AI_REPORT_REMOTE_PATH -ErrorAction SilentlyContinue
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits $commit -Source manual
```

通过标准：

- 输出中不再出现“请配置 `GIT_AI_REPORT_REMOTE_URL`”之类的提示
- 输出或 trace 中出现最终地址 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`
- 远端返回 200，或批量响应里的 `results[]` 能看到对应 commit 的成功状态
- 如果测试环境要求认证，请确保 `GIT_AI_REPORT_REMOTE_API_KEY` 有效；如不要求认证，可省略该变量

### 6. 验证 `/speckit.code-review` 会触发同一接口上传

```Plain Text
在 VS Code Agent 会话中执行：/speckit.code-review
```

通过标准：

- 本次审查报告末尾生成 AI 统计表格
- Code Review 过程日志中能看到 `.specify/scripts/powershell/upload-ai-stats.ps1 -Commits "..."` 被调用
- 上传脚本日志或网关日志显示目标地址仍为 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`
- 若服务端可查日志，应能看到与本次 reviewDocumentId / commit SHA 对应的请求记录

### 7. 可选：验证离线打包脚本不再触发 ZipArchiveHelper 文件占用错误

```PowerShell
cd D:\git-ai-main\spec-kit-standalone\spec-kit
.\.github\workflows\scripts\create-release-packages.ps1 -Version v0.0.0 -Agents copilot -Scripts ps
```

通过标准：

- 终端不再出现 `ZipArchiveHelper` 或 `Compress-Archive` 相关文件占用报错
- 若脚本后续因为本地目录命名或变体目录不存在而退出，应单独处理，不视为压缩实现回归

## 本次实测结果

### 安装验证结果

- 已执行 `uv tool uninstall specify-cli`
- 已执行 `uv tool install specify-cli --from "D:\git-ai-main\spec-kit-standalone\spec-kit" --force --no-cache`
- 安装结果显示 `specify-cli==0.0.79 (from file:///D:/git-ai-main/spec-kit-standalone/spec-kit)`

### 初始化验证结果

在 `D:\rj-op` 执行以下命令：

```PowerShell
specify init --here --force --ai copilot --script ps --offline
```

关键输出为：

```Plain Text
Apply bundled assets (bundled assets applied)
Run post-init hooks (ok)
Finalize (project ready)
Project ready.
```

结论：

- Windows 下缺少 `pwsh` 时，`post-init` 已能自动回退到 `powershell.exe`
- 本次实测中未再出现 `WinError 2`

### 冒烟验证结果

- `D:\rj-op\.specify\scripts\powershell\post-init.ps1 -Skip`：执行成功
- `git-ai --version`：返回 `1.3.0`

### 离线打包修复验证结果

- 已执行 `create-release-packages.ps1 -Version v0.0.0 -Agents copilot -Scripts ps`
- 原始问题中的 `ZipArchiveHelper` / `Compress-Archive` 文件占用报错未再出现

## 验证完成判定

满足以下 6 条即可判定“本地仓库验证通过”：

1. `specify-cli` 已经通过本地源码重新安装，并明确使用了 `--no-cache`
2. `specify init --here --force --ai copilot --script ps --offline` 可以完成初始化
3. `Run post-init hooks` 状态为 `ok`，且 `Project ready.` 正常输出
4. `post-init.ps1 -Skip` 与 `git-ai --version` 均可正常执行
5. 手动执行 `upload-ai-stats.ps1` 时，命中最终 AI 统计上传接口并返回成功
6. 执行 `/speckit.code-review` 时，AI 统计上传同样命中最终接口

## 失败时优先排查项

### `uv tool install` 后仍然是旧代码

- 不要只执行 `uv tool install --force --from ...`
- 先执行 `uv tool uninstall specify-cli`
- 再执行 `uv tool install specify-cli --from "本地源码路径" --force --no-cache`

### `specify init` 走到了远端模板而不是本地源码

- 本地源码验证必须带 `--offline`
- 不带 `--offline` 时，`specify init` 会优先走 GitHub release 资源，可能看不到本地刚修改的模板和脚本

### 仍出现 `Run post-init hooks ([WinError 2] 系统找不到指定的文件。)`

- 优先确认当前安装到 `uv` 工具目录里的 `specify-cli` 已经是最新本地构建结果
- 再确认机器上虽然没有 `pwsh`，但 `powershell.exe` 可用
- 若 `powershell.exe` 也不可用，则需要先修复本机 PowerShell 环境

### 仍出现 `Run post-init hooks (script not found)`

- 先确认是否带了 `--offline`
- 再确认本地 bundled assets 中已经包含 `.specify/scripts/powershell/post-init.ps1`

### 仍出现 `ZipArchiveHelper` / `Compress-Archive` 文件占用报错

- 先确认本地源码已包含最新的压缩实现修复
- 再确认当前执行的 `create-release-packages.ps1` 来自本地最新仓库，而不是旧副本

### 主动上传仍提示未配置 remote URL 或命中了错误地址 / 返回 404

- 先确认 `.specify/scripts/powershell/upload-ai-stats.ps1` 已同步到最新版本，默认路径应为 `/api/ai-cr-manage-service/api/public/upload/ai-stats`
- 再确认当前 shell 中没有残留旧的 `GIT_AI_REPORT_REMOTE_URL`、`GIT_AI_REPORT_REMOTE_ENDPOINT`、`GIT_AI_REPORT_REMOTE_PATH`
- 若怀疑被用户级环境变量覆盖，可先在当前进程里清空上述变量，再重新执行验证命令

### `/speckit.code-review` 没有触发 AI 统计上传

- 先确认本地模板已经同步到最新版本，尤其是 `.specify/templates/commands/code-review.md` 对应的上传步骤
- 再确认 `git-ai --version` 正常、待审查 commit 能返回 `git-ai stats <sha> --json`
- 若报告正常生成但没有 AI 统计表格，优先检查 Agent 日志里是否跳过了上传脚本，或脚本返回了远端错误

> （注：文档内容基于 2026-04-18 本地真实安装与验证过程整理）