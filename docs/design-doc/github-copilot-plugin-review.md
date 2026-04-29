# Code Review 报告：spec-kit-standalone git-ai 集成实现 vs 设计文档

**审查日期：** 2026-04-23  
**需求文档：** `git-ai/docs/speckit-gitai-integration-plan(3)(1).md`  
**审查范围：** `spec-kit-standalone/spec-kit/` 目录下与 git-ai 集成相关的全部实现  
**审查维度：** 需求符合性 + 代码质量

---

## 一、审查摘要

### 总体评价

实现基本覆盖了设计文档的两大核心需求（需求1：自动安装 git-ai，需求2：AI 统计上传），整体架构与文档描述一致。**但存在若干偏离和遗漏**，按严重程度列举如下。

### 统计

| 类型 | 数量 |
|------|------|
| 🔴 严重（需求偏离/代码质量） | 2 |
| 🟡 一般（需求偏离/代码质量） | 5 |
| 🔵 优化建议 | 3 |

---

## 二、需求 1 审查：Speckit 自动安装 git-ai

### ✅ 已正确实现的部分

| 检查项 | 状态 | 说明 |
|--------|------|------|
| `scripts/powershell/post-init.ps1` 模板源 | ✅ | 存在，逻辑与设计文档原型代码一致 |
| `scripts/bash/post-init.sh` 模板源 | ✅ | 存在，bash 版本行为与 PowerShell 对齐 |
| `.specify/scripts/powershell/post-init.ps1` 仓库副本 | ✅ | 存在，与模板源内容完全一致 |
| `test-verify/.specify/scripts/powershell/post-init.ps1` 验证副本 | ✅ | 存在，与模板源内容完全一致 |
| `src/specify_cli/__init__.py` post-init 入口 | ✅ | `_get_post_init_command()`、`_resolve_post_init_shell()`、`run_post_init_script()` 均已实现 |
| `init()` 调用点正确 | ✅ | 放在 preset 安装之后、`tracker.complete("final")` 之前，符合设计 |
| post-init 失败不阻塞 init | ✅ | `run_post_init_script()` 内部 try/catch，只 warning 不 raise |
| `check-prerequisites.ps1` git-ai 兜底检测 | ✅ | 已实现 warning 提示，不阻塞流程 |
| `-Force` / `-Skip` 参数支持 | ✅ | PowerShell 和 bash 版本均支持 |
| `GIT_AI_INSTALLER_URL` 覆盖 | ✅ | 两个版本都支持环境变量覆盖 |
| TLS 1.2 强制 | ✅ | PowerShell 版本已设置 `SecurityProtocol` |
| 三份文件一致性 | ✅ | `scripts/`、`.specify/`、`test-verify/.specify/` 三处 post-init.ps1 完全一致 |

### 实现增强（超出设计但合理）

1. **增加了 `Write-PostInitDetail` 函数**：提供 DarkGray 色调的详细诊断日志，便于排查问题。设计文档原型无此功能，属于合理增强。
2. **bash 版本更健壮的下载工具检测**：同时支持 `curl` 和 `wget`，若都不存在则 warn。
3. **已安装 + Force 场景分离**：当 git-ai 已安装且指定 `-Force` 时，先显示当前版本再重新安装，体验更好。

---

## 三、需求 2 审查：AI 统计上传

### ✅ 已正确实现的部分

| 检查项 | 状态 | 说明 |
|--------|------|------|
| `upload-ai-stats.ps1` 脚本 | ✅ | 存在，4 种使用场景（默认/日期/指定commit/DryRun）均支持 |
| 批量上传（单次 POST） | ✅ | `Send-AiStatsBatchToRemote` 一次发送所有 commit |
| snake_case → camelCase 转换 | ✅ | `Convert-ObjectKeysToCamelCase` 递归转换 |
| `toolModelBreakdown` 展开 | ✅ | `Convert-ToolModelBreakdownToDto` 正确展开为数组 |
| `hasAuthorshipNote` 标记 | ✅ | 通过 `git notes --ref=ai list` 一次性构建 lookup |
| 无 note 的 commit 不跳过 | ✅ | 符合设计要求，标记 `hasAuthorshipNote=false` 继续处理 |
| 默认 remote URL 内置 | ✅ | 默认 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats` |
| 环境变量覆盖体系 | ✅ | `GIT_AI_REPORT_REMOTE_URL` / `ENDPOINT` + `PATH` / `API_KEY` / `USER_ID` 全部支持 |
| `Invoke-ProcessCapture` UTF-8 编码 | ✅ | 设置了 `StandardOutputEncoding`/`StandardErrorEncoding` 为 UTF-8 |
| 三份文件一致性 | ✅ | `scripts/`、`.specify/`、`test-verify/.specify/` 三处 upload-ai-stats.ps1 完全一致 |
| 10 秒超时 | ✅ | `Invoke-RestMethod -TimeoutSec 10` |
| API Key 不进仓库 | ✅ | 仅通过环境变量读取 |
| 批量响应逐 commit 解析 | ✅ | `Convert-BatchUploadResponse` 正确映射 `results[]` |
| JSON 输出模式 | ✅ | `-Json` 开关抑制 Write-Host，最终输出纯 JSON |

### 实现增强（超出设计但合理）

1. **X-USER-ID 自动探测**：从 VS Code/IDEA MCP 配置中自动读取 `X-USER-ID`，设计文档 3.6 节提到了此功能，实现完整覆盖了 VS Code (`mcp.json`/`settings.json`) 和 IDEA (`github-copilot/intellij/mcp.json`/JetBrains 目录扫描) 两条路径。
2. **JSONC 解析**：`ConvertFrom-JsonWithComments` 手动剥离注释和尾逗号，兼容 VS Code settings.json 格式。
3. **`-LogHttpPayload` 开关**：调试时可打印完整请求 URL、headers（密钥脱敏）和 body。
4. **`GIT_AI_REPORT_LOG_REQUEST` 环境变量**：也可通过环境变量开启请求日志。

---

## 四、问题清单

### 🔴 P1：`Get-CommitAiFileStats` 使用 `git-ai diff` 而非设计要求的 authorship note 解析

**问题描述：**

设计文档在 §3.3 中明确指出逐文件统计应使用 **commit-local 语义**：直接解析 `git notes --ref=ai show <sha>` 的 attestation 段，结合 `git diff-tree --numstat`，**不应使用 `git-ai diff`**。

原文引用：
> "为什么不用 git-ai diff？git-ai diff 是 provenance-traced，会跨 commit 追溯行的来源……这不符合 commit-local 的业务语义。"

**实际实现：**

`upload-ai-stats.ps1` 第 1039 行：
```powershell
$diffCommandResult = Invoke-ProcessCapture -FilePath 'git-ai' -Arguments @('diff', $CommitSha, '--json', '--include-stats')
```

实现使用了 `git-ai diff --json --include-stats`，这是 provenance-traced 语义，可能产生与设计文档不同的归因结果。例如：某个 commit 中纯人工新增的行，如果其历史来源是 AI commit，`git-ai diff` 会把它归为 AI，而 commit-local 语义应归为人工。

**影响：**
- 数据语义偏离设计：文件级 AI/人工比例可能与 commit 级统计（来自 `git-ai stats`）不一致
- 当团队依赖此数据做审查决策时，可能产生误导

**修复方案：**

按设计文档实现，改为解析 authorship note attestation：

```powershell
# ❌ 当前实现
$diffCommandResult = Invoke-ProcessCapture -FilePath 'git-ai' -Arguments @('diff', $CommitSha, '--json', '--include-stats')

# ✅ 应改为
# Step 1: git diff-tree --numstat
$numstatResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'diff-tree', '--no-commit-id', '--numstat', '-r', $CommitSha)

# Step 2: git notes --ref=ai show <sha>
$noteResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'notes', '--ref=ai', 'show', $CommitSha)

# Step 3: 解析 attestation 段（非缩进行=文件路径，缩进行="<id> <range>"归因条目）
# h_* 前缀 = 人工，其他 = AI prompt hash
```

设计文档 §3.3 中已提供了完整的 PowerShell 参考实现（`Get-CommitAiFileStats` 函数），可直接对齐。

---

### 🔴 P2：`authorshipSchemaVersion` 与设计文档不一致

**问题描述：**

设计文档 §3.5 明确定义批量请求体中 `authorshipSchemaVersion` 为 `"authorship/3.0.0"`，作为固定值用于服务端兼容不同版本。

**实际实现：**

`upload-ai-stats.ps1` 第 1403 行：
```powershell
authorshipSchemaVersion = 'authorship/3.1.0'
```

版本号从 `3.0.0` 变为 `3.1.0`，且未见相关变更说明。

**影响：**
- 服务端如果严格按 `3.0.0` 做版本匹配，会拒绝 `3.1.0` 的请求
- 前后端版本协议不一致可能导致数据解析错误

**修复方案：**

确认是否有意升级版本号。如果是因为使用了 `git-ai diff` 导致数据结构变化而升级，则修复 P1 后应回退为 `3.0.0`；如果确实有新增字段需要 `3.1.0`，应在设计文档中补充变更说明。

```powershell
# 与设计文档对齐
authorshipSchemaVersion = 'authorship/3.0.0'
```

---

### 🟡 P3：缺少 `upload-ai-stats.sh` bash 版本

**问题描述：**

设计文档 §3.3 的上传脚本以 PowerShell 为主实现，但整个集成方案支持 `--script sh` 路径（bash）。`post-init.sh` 已有 bash 对应版本，而 `upload-ai-stats.ps1` 没有 bash 版本。

**影响：**
- macOS/Linux 用户在 bash 环境下无法直接使用主动上传功能
- Code Review Agent 步骤 8.4 在 bash 环境下无法调用上传脚本

**建议：**

Phase 2 或 Phase 3 阶段补齐 `scripts/bash/upload-ai-stats.sh`，并同步副本。

---

### 🟡 P4：`check-prerequisites.sh` 缺少 git-ai 检测

**问题描述：**

PowerShell 版 `check-prerequisites.ps1` 已按设计文档要求添加了 git-ai 兜底检测（第 59-72 行），但 bash 版 `check-prerequisites.sh` 中未找到相应的 git-ai 检测逻辑。

**影响：**
- bash 环境下的存量项目不会收到 git-ai 缺失的 warning 提示

**修复方案：**

在 `scripts/bash/check-prerequisites.sh` 中 source common.sh 之后添加类似逻辑：

```bash
# ── git-ai installation check (fallback for legacy projects) ──
if ! command -v git-ai &>/dev/null; then
    echo "WARNING: =================================================="  >&2
    echo "WARNING:   [speckit] git-ai not detected!"                    >&2
    echo "WARNING:   AI code attribution will not be available."        >&2
    echo "WARNING:"                                                     >&2
    echo "WARNING:   Install by running:"                               >&2
    echo "WARNING:   .specify/scripts/bash/post-init.sh"                >&2
    echo "WARNING: =================================================="  >&2
fi
```

---

### 🟡 P5：`Invoke-ProcessCapture` 中 `Process.Start()` 后存在死锁风险

**问题描述：**

`upload-ai-stats.ps1` 中的 `Invoke-ProcessCapture` 函数：

```powershell
$stdout = $process.StandardOutput.ReadToEnd()
$stderr = $process.StandardError.ReadToEnd()
$process.WaitForExit()
```

当 stdout 和 stderr 的缓冲区都满时，先同步读 stdout 会阻塞等待子进程写完 stdout，但如果子进程先往 stderr 写满了缓冲区也会阻塞，导致双向死锁。

**影响：**
- 当 `git-ai stats` 或 `git-ai diff` 输出大量数据到 stderr（如大量 warning）时，可能导致脚本挂起
- 生产环境中可能很少触发，但在异常场景下（如大量文件变更）存在风险

**修复方案：**

使用异步读取模式避免死锁：

```powershell
# ✅ 安全的异步读取模式
[void]$process.Start()
$stdoutTask = $process.StandardOutput.ReadToEndAsync()
$stderrTask = $process.StandardError.ReadToEndAsync()
$process.WaitForExit()
$stdout = $stdoutTask.GetAwaiter().GetResult()
$stderr = $stderrTask.GetAwaiter().GetResult()
```

或使用 `BeginOutputReadLine`/`BeginErrorReadLine` 事件模式。

---

### 🟡 P6：`New-CommitUploadItem` 使用 `|` 分隔 git log 输出存在注入风险

**问题描述：**

```powershell
$commitInfoResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'log', '-1', '--format=%ae|%s|%aI', $CommitSha)
$parts = $commitInfo -split '\|', 3
```

如果 commit message 中包含 `|` 字符，`-split '\|', 3` 的第三个参数限制虽然保护了 timestamp，但 `$parts[1]`（commitMessage）会被截断。

**影响：**
- commit message 如 `feat: add A|B selection` 会被截断为 `feat: add A`
- 这是数据准确性问题，不是安全漏洞

**修复方案：**

使用更不常见的分隔符或多次 git log 调用：

```powershell
# ✅ 使用 NUL 字符或其他罕见分隔符
$commitInfoResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'log', '-1', '--format=%ae%x00%s%x00%aI', $CommitSha)
$parts = $commitInfo -split "`0", 3
```

---

### 🟡 P7：`Get-UploadRemoteConfig` 在默认 URL 场景下不再返回 `$null`

**问题描述：**

设计文档中的 `Get-UploadRemoteConfig` 在未配置任何环境变量时会返回 `$null` 并打印 warning，主流程检测到 `$null` 后 `exit 1`。

实际实现中，当未设置任何 URL 相关环境变量时，脚本回退到内置默认 URL（`$script:DefaultRemoteUrl`），**永远不会返回 `$null`**。

**影响：**
- 这实际上是**更好的行为**（开箱即用），但与设计文档描述不一致
- 如果内置默认 URL 不可达，用户不会在配置阶段得到提示，而是在上传时才发现失败

**建议：**

更新设计文档以反映当前默认 URL 回退行为，或在脚本中添加首次连接检测。

---

### 🔵 P8：`post-init.ps1` 成功安装后未检查 `$LASTEXITCODE`

**问题描述：**

```powershell
Invoke-GitAiInstaller
# ...
$resolvedCommand = Get-GitAiCommand
```

`Invoke-GitAiInstaller` 中调用 `& $tempInstaller`，但未检查 `$LASTEXITCODE`。如果安装器脚本退出码非零但未抛出异常（PowerShell 中 native command 退出码 ≠ 0 不会自动抛异常），脚本会继续到 `Get-GitAiCommand` 阶段，可能报告"安装成功"但实际 git-ai 未正确安装。

**建议：**

```powershell
& $tempInstaller
if ($LASTEXITCODE -ne 0) {
    throw "git-ai installer exited with code $LASTEXITCODE"
}
```

---

### 🔵 P9：`Remove-JsonCommentText` 未处理单引号字符串

**问题描述：**

`upload-ai-stats.ps1` 中的 JSONC 解析器 `Remove-JsonCommentText` 仅跟踪双引号字符串内的状态。虽然 JSON 标准不支持单引号字符串，但某些 IDE 配置文件（特别是旧版 VS Code settings）可能在注释示例中包含单引号，极端场景下可能误判注释边界。

**影响：** 极低概率，标准 JSON/JSONC 不使用单引号。

**建议：** 保持现状即可，仅记录为已知边界。

---

### 🔵 P10：`JetBrains` 目录扫描的性能风险

**问题描述：**

`Get-IdeaMcpConfigPaths` 函数扫描整个 `%APPDATA%/JetBrains` 和 `%LOCALAPPDATA%/JetBrains` 目录的所有 `.json` 文件：

```powershell
Get-ChildItem -Path $jetBrainsRoot -Recurse -File -Filter *.json -ErrorAction SilentlyContinue | Where-Object {
    $_.Length -gt 0 -and $_.Length -lt 1048576
}
```

JetBrains 目录下可能有大量插件缓存和配置文件。

**影响：**
- 首次调用时可能有几秒的 IO 延迟
- 已有 `$_.Length -lt 1048576` 过滤，缓解了大文件读取问题

**建议：**

考虑限制扫描深度（`-Depth 3`）或优先匹配已知路径模式。

---

## 五、文件清单与覆盖情况

| 设计文档要求的文件 | 是否存在 | 内容正确性 |
|-------------------|---------|-----------|
| `scripts/powershell/post-init.ps1` | ✅ | ✅ 符合 |
| `scripts/bash/post-init.sh` | ✅ | ✅ 符合 |
| `.specify/scripts/powershell/post-init.ps1` | ✅ | ✅ 与源一致 |
| `test-verify/.specify/scripts/powershell/post-init.ps1` | ✅ | ✅ 与源一致 |
| `src/specify_cli/__init__.py`（post-init hooks） | ✅ | ✅ 符合 |
| `scripts/powershell/check-prerequisites.ps1`（git-ai 检测） | ✅ | ✅ 符合 |
| `scripts/bash/check-prerequisites.sh`（git-ai 检测） | ✅ 存在 | ⚠️ 缺少 git-ai 检测（P4） |
| `scripts/powershell/upload-ai-stats.ps1` | ✅ | ⚠️ 逐文件统计方法偏离（P1） |
| `.specify/scripts/powershell/upload-ai-stats.ps1` | ✅ | ✅ 与源一致 |
| `test-verify/.specify/scripts/powershell/upload-ai-stats.ps1` | ✅ | ✅ 与源一致 |
| `scripts/bash/upload-ai-stats.sh` | ❌ 缺失 | — （P3） |

---

## 六、总结与修复优先级

| 优先级 | 问题 | 建议行动 |
|--------|------|---------|
| 🔴 P1 | `Get-CommitAiFileStats` 使用 `git-ai diff` 而非 attestation 解析 | 按设计文档改为 commit-local 语义 |
| 🔴 P2 | `authorshipSchemaVersion` 为 `3.1.0` 而非文档约定的 `3.0.0` | 确认意图后对齐或更新文档 |
| 🟡 P3 | 缺少 `upload-ai-stats.sh` bash 版本 | 后续 Phase 补齐 |
| 🟡 P4 | bash `check-prerequisites.sh` 缺少 git-ai 检测 | 补充 git-ai 检测逻辑 |
| 🟡 P5 | `Invoke-ProcessCapture` 存在 stdout/stderr 死锁风险 | 改为异步读取 |
| 🟡 P6 | git log `|` 分隔符可能截断 commit message | 改用 `%x00` 分隔符 |
| 🟡 P7 | 默认 URL 回退行为与文档描述不一致 | 更新设计文档 |
| 🔵 P8 | `post-init.ps1` 安装后未检查退出码 | 添加 `$LASTEXITCODE` 检查 |
| 🔵 P9 | JSONC 解析器未处理单引号 | 保持现状 |
| 🔵 P10 | JetBrains 目录扫描性能 | 考虑限制扫描深度 |

---

**审查人：** GitHub Copilot (AI-assisted review)  
**审查方式：** 基于设计文档逐项对照代码实现
