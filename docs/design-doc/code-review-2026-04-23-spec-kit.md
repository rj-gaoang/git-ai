# 代码审查报告：spec-kit-standalone (基于需求文档符合性审查)


## 用户输入的原始提示词
`& 'd:\git-ai-main\git-ai\docs\speckit-gitai-integration-plan(3)(1).md'是需求和设计文档，根据这份文档review spec-kit-standalone文件夹下的spec-kit，review结果形成.md文档`

## 用户评审所选择的模型名称
claude-opus-4-6

## ✅ 优点
- post-init hook 实现完整，PowerShell 和 Bash 双平台支持，失败不阻断 `specify init` 流程
- `upload-ai-stats.ps1` 功能丰富，支持多种参数模式（`-Since`/`-Until`/`-Commits`/`-DryRun`/`-Json`），错误处理和日志输出规范
- X-USER-ID 解析链实现完整：环境变量 → VS Code MCP 配置 → IDEA MCP 配置，含 JSONC 注释剥离
- Python CLI 的 post-init 集成设计合理，StepTracker 状态追踪清晰，120s 超时保护到位
- `post-init.ps1` / `post-init.sh` 三处副本（`scripts/`、`.specify/`、`test-verify/.specify/`）内容完全一致

## 📋 提交概览
| 项目 | 内容 |
|------|------|
| 审查日期 | 2026-04-23 |
| 审查类型 | 需求文档符合性审查（非 commit-based） |
| 需求文档 | `docs/speckit-gitai-integration-plan(3)(1).md` (2285行) |
| 审查目标 | `D:\git-ai-main\spec-kit-standalone\spec-kit\` |
| 涉及文件 | `__init__.py`, `post-init.ps1`, `post-init.sh`, `upload-ai-stats.ps1`, `common.ps1` 等 |

## 📄 需求文档摘要

> 本次代码审查基于 `speckit-gitai-integration-plan(3)(1).md` 需求设计文档进行符合性检查。

### 需求来源
- **文档名称**: `docs/speckit-gitai-integration-plan(3)(1).md`
- **提取日期**: 2026-04-23

### 核心需求
| 类别 | 需求内容 |
|------|---------|
| 📋 **功能需求** | 1. `specify init` 后自动安装 git-ai（post-init hook）<br>2. 手动上传 AI 统计数据（`upload-ai-stats.ps1`）<br>3. Code Review 自动上传（步骤 8.3/8.4） |
| 🔗 **接口定义** | 单次 POST 批量上传，`commits[]` 数组，服务端按 `(repoUrl, commitSha)` 做 upsert |
| 🔒 **安全要求** | X-USER-ID 通过环境变量 / IDE MCP 配置解析 |
| 📊 **数据要求** | `authorshipSchemaVersion: "authorship/3.0.0"`；逐文件统计使用 commit-local 语义 |
| 🔄 **业务流程** | 逐文件统计：`git notes --ref=ai show <sha>` + `git diff-tree --numstat`（commit-local，非 provenance-traced） |

### 需求符合性总结
- ✅ **已满足**: post-init hook 双平台实现、X-USER-ID 解析链、批量上传 API 对接、`git-ai stats` 汇总统计
- ⚠️ **部分满足**: 逐文件统计数据源（使用了 provenance-traced 而非 commit-local）
- ❌ **未满足**: `Get-CommitAiFileStats` 未按需求改用 authorship note 直接解析
- 📊 **符合率**: 约 75%

---

## ⚠️ 问题清单

### 🔴 P0-需求偏离 - 严重

**问题分类:** 数据语义错误 - 逐文件统计使用了错误的数据源

**问题描述:** `Get-CommitAiFileStats` 函数使用 `git-ai diff <sha> --json --include-stats`（provenance-traced 语义）获取逐文件 AI 归因数据，而需求文档明确要求使用 commit-local 语义：直接解析 `git notes --ref=ai show <sha>` 的 attestation 段 + `git diff-tree --numstat`。

**风险级别:** 🔴P0-需求偏离-严重

**问题代码位置:** spec-kit-standalone/spec-kit/scripts/powershell/upload-ai-stats.ps1:1036-1151

**需求要求:**
需求文档第 85 行明确指出：
> 逐文件统计使用 commit-local 语义：直接解析 `git notes --ref=ai show <sha>` 的 attestation 段，结合 `git diff-tree --numstat` 获取每个文件的新增/删除行数，无需调用 `git-ai diff`（后者是 provenance-traced，会跨 commit 追溯，不适合 commit-local 场景）

需求文档第 850-854 行解释了原因：
> `git-ai diff` 是 provenance-traced，会跨 commit 追溯行的来源。例如 commit A 是纯人工，但其中某些行最初来自更早的 AI commit，`git-ai diff` 会把它们归为 AI。这不符合 commit-local 的业务语义。

需求文档变更日志（第 2247-2284 行）已明确记录此变更决策及验证结果。

**需求偏离说明:** 当前实现调用 `git-ai diff`（provenance-traced），会导致纯人工 commit 中的某些行被错误标记为 AI 行（因为这些行在更早的 commit 中由 AI 生成过）。这直接违背了 "这个 commit 本身有多少 AI 参与" 的业务语义。

**影响说明:** AI 归因数据不准确。纯人工 commit 可能被错误报告为含有 AI 代码，影响统计数据的可信度和业务决策。

**修复方案:**
```powershell
# ❌ 当前实现（provenance-traced，upload-ai-stats.ps1:1036-1039）
function Get-CommitAiFileStats {
    param([string]$CommitSha)
    $diffCommandResult = Invoke-ProcessCapture -FilePath 'git-ai' -Arguments @('diff', $CommitSha, '--json', '--include-stats')
    # ... 解析 git-ai diff 输出
}

# ✅ 需求要求的实现（commit-local 语义，参考需求文档第 856-899 行的参考实现）
function Get-CommitAiFileStats {
    param([string]$CommitSha)
    $repoRoot = Get-RepoRoot

    # Step 1: 每个文件的 added/deleted 行数
    $numstatResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'diff-tree', '--no-commit-id', '--numstat', '-r', $CommitSha)
    if ($numstatResult.ExitCode -ne 0 -or -not $numstatResult.StdOut) { return @() }

    # Step 2: 读取 authorship note（commit-local）
    $noteResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'notes', '--ref=ai', 'show', $CommitSha)
    # 解析 attestation 段 + JSON 元数据段...
}
```

**新引入:** ❌否（需求文档变更日志记录此为已知待修复项）

---

### 🔴 P0-严重问题（必须修复）

**问题分类:** 文件同步不一致 - `common.ps1` 三处副本内容不同

**问题描述:** `scripts/powershell/common.ps1` 与 `.specify/scripts/powershell/common.ps1` 内容不一致。`scripts/` 和 `test-verify/.specify/` 下的副本一致（MD5: `7ee7c4853a385896fe769243f4dd8f28`），但 `.specify/` 下的副本是旧版本（MD5: `4ddd717dc8223f39edf40a899980ff75`），缺少 `Find-SpecifyRoot`、`Resolve-Template`、时间戳分支支持、`-LiteralPath` 用法等功能。

**风险级别:** 🔴P0-严重

**问题代码位置:** spec-kit-standalone/spec-kit/.specify/scripts/powershell/common.ps1

**影响说明:** 需求文档要求三处副本保持一致（`scripts/` 为模板源，`.specify/` 和 `test-verify/.specify/` 为同步副本）。`.specify/` 下的旧版 `common.ps1` 可能导致 `upload-ai-stats.ps1` 在 `.specify/` 路径下执行时缺少必要的公共函数，引发运行时错误。

**建议:**
将 `scripts/powershell/common.ps1` 的内容同步到 `.specify/scripts/powershell/common.ps1`，确保三处副本完全一致。

```powershell
# 同步命令
Copy-Item "scripts/powershell/common.ps1" ".specify/scripts/powershell/common.ps1" -Force
```

**新引入:** ✅是

---

### 🟡 P2-需求偏离 - 一般

**问题分类:** 版本号不一致 - `authorshipSchemaVersion` 与需求文档不符

**问题描述:** `upload-ai-stats.ps1` 中 `authorshipSchemaVersion` 使用 `'authorship/3.1.0'`，而需求文档明确要求固定值 `"authorship/3.0.0"`。

**风险级别:** 🟡P2-需求偏离-一般

**问题代码位置:** spec-kit-standalone/spec-kit/scripts/powershell/upload-ai-stats.ps1:1403

**需求要求:** 需求文档第 1796 行：`authorshipSchemaVersion` 数据格式版本，固定值 `"authorship/3.0.0"`，服务端兼容不同版本用。

**当前实现:** `authorshipSchemaVersion = 'authorship/3.1.0'`

**偏离说明:** 版本号从 `3.0.0` 升级到 `3.1.0`，可能是实现过程中的有意升级（因数据结构变化），但与需求文档不一致。如果服务端严格校验版本号，可能导致上传失败或数据解析异常。

**建议:** 确认服务端是否已支持 `3.1.0`。若已支持，需同步更新需求文档；若未支持，需回退为 `3.0.0`。

```powershell
# ❌ 当前实现
authorshipSchemaVersion = 'authorship/3.1.0'

# ✅ 与需求文档一致
authorshipSchemaVersion = 'authorship/3.0.0'
```

**新引入:** ✅是

---

### 🟡 P2-一般问题（建议修复）

**问题分类:** 跨平台支持缺失 - 缺少 `upload-ai-stats.sh` Bash 版本

**问题描述:** `post-init` 脚本同时提供了 PowerShell（`post-init.ps1`）和 Bash（`post-init.sh`）两个版本，但 `upload-ai-stats` 仅有 PowerShell 版本，缺少 Bash 等效实现。

**风险级别:** 🟡P2-一般

**问题代码位置:** spec-kit-standalone/spec-kit/scripts/powershell/upload-ai-stats.ps1（无对应 bash/ 目录下的等效脚本）

**影响说明:** macOS/Linux 用户如果未安装 PowerShell（pwsh），将无法使用手动上传功能。虽然 Code Review 自动上传路径（步骤 8.3/8.4）不依赖此脚本，但手动上传场景受限。

**建议:** 考虑提供 `scripts/bash/upload-ai-stats.sh` 的 Bash 等效实现，或在文档中明确说明此脚本仅支持 PowerShell 环境。

**新引入:** ✅是

---

### 🟡 P2-一般问题（建议修复）

**问题分类:** 终端输出冲突 - `subprocess.run` 在 Rich Live 上下文中执行

**问题描述:** Python CLI 的 `run_post_init_script` 函数中，`subprocess.run` 未设置 `capture_output=True`，子进程的 stdout/stderr 会直接输出到终端。如果调用方处于 Rich `Live` 上下文中，子进程输出可能与 Rich 的动态渲染产生视觉冲突。

**风险级别:** 🟡P2-一般

**问题代码位置:** spec-kit-standalone/spec-kit/src/specify_cli/__init__.py:1607-1611

**影响说明:** post-init 脚本（如 git-ai 安装过程）的输出可能与 Rich 进度条/状态指示器交错显示，导致终端输出混乱。功能不受影响，但用户体验下降。

**建议:**
```python
# ❌ 当前实现
result = subprocess.run(
    cmd,
    cwd=str(project_path),
    timeout=120,
)

# ✅ 捕获输出后统一打印
result = subprocess.run(
    cmd,
    cwd=str(project_path),
    timeout=120,
    capture_output=True,
    text=True,
)
if result.stdout:
    console.print(result.stdout, highlight=False)
if result.stderr:
    console.print(f"[dim]{result.stderr}[/dim]", highlight=False)
```

**新引入:** ✅是

---

### 🔵 P3-优化建议

**问题描述:** `_get_post_init_skip_reason` 函数中存在不可达代码

**风险级别:** 🔵P3-需求优化

**问题分类:** 代码质量 - 死代码

**问题代码位置:** spec-kit-standalone/spec-kit/src/specify_cli/__init__.py:1562

**当前实现:** 函数在 `script_type == "ps"` 和 `script_type == "sh"` 两个分支后有一个 `return "launcher not found"` 兜底返回。但由于 `SCRIPT_TYPE_POST_INIT` 字典只包含 `"ps"` 和 `"sh"` 两个 key，且函数开头已对未知 key 返回 `"unsupported script type"`，因此第 1562 行永远不会被执行。

**优化方案:**
```python
# ❌ 当前实现（第 1558-1562 行）
if script_type == "ps":
    return "PowerShell not found"
if script_type == "sh":
    return "bash not found"
return "launcher not found"  # 不可达

# ✅ 移除死代码
if script_type == "ps":
    return "PowerShell not found"
return "bash not found"
```

**优化收益:** 消除死代码，提高代码可读性。

**新引入:** ✅是

---

### 🔵 P3-优化建议

**问题描述:** `specify init` 完成后未将 `init-options.json` 纳入初始 git commit

**风险级别:** 🔵P3-需求优化

**问题分类:** 工作流完整性

**问题代码位置:** spec-kit-standalone/spec-kit/src/specify_cli/__init__.py（init 函数末尾）

**当前实现:** `specify init` 在 `save_init_options()` 步骤生成 `.specify/init-options.json`，但该文件未被自动加入 git 暂存区。用户需要手动 `git add` 此文件。

**优化方案:** 在 init 流程的 git commit 步骤中，将 `init-options.json` 一并纳入提交范围。

**优化收益:** 减少用户手动操作，确保项目初始化配置被版本控制追踪。

**新引入:** ✅是

---

## 📊 总体评价

| 评估项 | 评分/说明 |
|--------|----------|
| 代码质量 | 7/10分 |
| 需求符合度 | 6/10分 |
| 主要优点 | 1. post-init hook 双平台实现完整，容错设计合理<br>2. upload-ai-stats.ps1 功能丰富，参数设计灵活<br>3. X-USER-ID 多源解析链实现完善 |
| 主要问题 | 1. `Get-CommitAiFileStats` 数据源语义错误（provenance-traced vs commit-local）<br>2. `common.ps1` 三处副本不同步<br>3. `authorshipSchemaVersion` 版本号与需求不一致 |
| 需求偏离项 | 1. 逐文件统计未改用 commit-local 语义（🔴严重）<br>2. `authorshipSchemaVersion` 为 3.1.0 而非 3.0.0（🟡一般） |

---

## 附录：审查说明

- **审查范围**: 基于需求设计文档 `speckit-gitai-integration-plan(3)(1).md` 对 `spec-kit-standalone/spec-kit/` 实现代码进行符合性审查
- **审查标准**:
  - 需求符合性：对比需求文档中的设计规格、数据语义、文件同步要求
  - 代码质量：参考通用代码审查标准（死代码、终端输出、跨平台支持）
- **问题风险级别**:
  - 需求符合性：🔴P0-需求偏离-严重 / 🟡P2-需求偏离-一般
  - 代码质量：🔴P0-严重 / 🟡P2-一般 / 🔵P3-建议优化
- **需求文档**: `D:\git-ai-main\git-ai\docs\speckit-gitai-integration-plan(3)(1).md` (2285行)
