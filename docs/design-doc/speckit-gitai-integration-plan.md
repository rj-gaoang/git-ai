# git-ai × Speckit 集成方案（详细实施指南）

## 0. 阅读本文前你需要知道的背景

### 什么是 git-ai？

git-ai 是一个 Rust 写的命令行工具，它作为 `git` 的透明代理运行。当团队成员在本地安装 git-ai 后，每次执行 `git commit`，git-ai 会自动拦截，在提交前后分析代码变更：哪些行是 AI 生成的（比如 Copilot、Cursor 产生的代码），哪些行是人手写的。分析结果以 Git Note 的形式存储在 `refs/notes/ai` 命名空间中。

**通俗理解：** git-ai 就像一个"代码 DNA 检测仪"，自动标记每行代码的来源是 AI 还是人类。

### 什么是 Speckit？

Speckit 是团队使用的「规范驱动开发」框架，通过 `.specify/` 目录管理需求规格、实施计划、任务分解和 Code Review 流程。它配合 VS Code Agent 使用，通过 `/speckit.specify`、`/speckit.code-review` 等命令驱动完整的开发流程。

**通俗理解：** Speckit 是团队的"研发流水线管理器"，从需求到 Code Review 全覆盖。

### 为什么要把二者集成？

| 问题 | 不集成时 | 集成后 |
|------|---------|--------|
| AI 代码追踪 | 需要每个人自己手动安装 git-ai，大部分人不会装 | 安装 Speckit 就自动装好 git-ai，零门槛 |
| AI 数据汇报 | 数据只存在本地，团队 leader 看不到 | 提供命令主动上传 + Code Review 自动上传到团队仪表盘 |
| Code Review 盲区 | Reviewer 不知道被审代码有多少是 AI 写的 | 审查报告自动附带 AI 占比数据，辅助判断审查重点 |

### 本文要解决的两个需求

**需求 1：** 团队成员安装或更新 Speckit 时，尽可能同步安装或更新 git-ai 并配置好 hooks。  
**需求 2：** 提供两种方式将 AI 检测结果上传到远程服务器——(A) 用户主动执行命令上传；(B) Code Review 时自动上传。

> **同步修订（2026-04-26）**：当前三条上传链路已经统一到同一个 payload 契约。
> 1. `git-ai` 被动上传、Speckit 手动上传、Code Review 自动上传都会带 commit 级 `prompts[]`。
> 2. 远端接口按 `commitSha` 去重，同一 commit 多次上传只保留一次主记录。
> 3. `git_ai_tool_stats` 继续按 `tool` / `model` 分列保存聚合统计，prompt 明细独立写入 `git_ai_prompt_stats`。
> 4. `git_ai_prompt_stats` 的 DDL 见 `docs/docs/plans/git_ai_prompt_stats.sql`。

---

## 一、现状分析——我们手里有什么牌？

> 在动手之前，先搞清楚两边已经提供了哪些能力，这样才知道新建什么、改什么、复用什么。

### 1.1 Speckit 侧已有的东西

Speckit 安装后会在项目根目录生成一个 `.specify/` 文件夹，里面包含：

```
.specify/
├── init-options.json                    ← 记录 Speckit 版本、脚本类型等初始化选项
├── scripts/powershell/
│   ├── common.ps1                       ← 公共函数库（查找项目根、获取分支名等）
│   ├── check-prerequisites.ps1          ← 前置检查（验证目录和文件是否就绪）
│   ├── create-new-feature.ps1           ← 创建功能分支
│   ├── setup-plan.ps1                   ← 复制计划模板
│   ├── batch-update.ps1                 ← 批量更新
│   ├── post-init.ps1                    ← 初始化/更新后安装或更新 git-ai
│   └── update-agent-context.ps1         ← 更新 Agent 上下文
└── templates/
    ├── spec-template.md                 ← 需求规格模板
    ├── plan-template.md                 ← 实施计划模板
    ├── tasks-template.md                ← 任务分解模板
    └── code-review/
        ├── template.md                  ← Code Review 报告模板
        ├── knowledge.md                 ← 问题分类决策树
        ├── backend-specification.md     ← 后端审查规范
        └── frontend-specification.md    ← 前端审查规范
```

**关键发现（影响设计决策）：**
- Speckit **没有** `postInit` 生命周期钩子，它的 init 流程就是拷贝模板 + 写 `init-options.json`，不会自动执行任何脚本
- 所有自动化依赖 **Agent prompt 指令** + **PowerShell 脚本**，这意味着我们也要用同样的模式来集成
- 如果想让 `specify init . --ai copilot` / `specify init --here --force --ai copilot` 这类 **CLI 初始化命令本身** 直接触发安装，最合理的做法是修改 `specify_cli/__init__.py` 的 `init()` 流程，在末尾补一个通用的 post-init 执行点
- `common.ps1` 提供了 `Get-RepoRoot`、`Test-HasGit` 等工具函数，我们的新脚本可以直接复用

### 1.2 git-ai 侧已有的东西

| 我们要用到的能力 | 它在哪里 | 它做什么 | 我们怎么用 |
|-----------------|---------|---------|-----------|
| **安装脚本** | `install.ps1`（Windows）/ `install.sh`（Unix） | 安装 git-ai，并在安装过程中自动执行 `git-ai install-hooks` | 在 Speckit init 时调用它来安装 |
| **Hooks 配置** | `git-ai install-hooks` 命令 | 配置 IDE / Agent hooks 与全局 git-ai 集成；这是当前推荐入口 | 对于已安装用户可重复执行，用于刷新集成配置 |
| **统计查询** | `git-ai stats <commit-sha> --json` 命令 | 读取 `refs/notes/ai`，输出该 commit 的 AI 使用 JSON 数据 | 上传脚本调用它获取每个 commit 的 **commit 级**统计 |
| **本地存储** | `refs/notes/ai`（Git Notes） | 每次 commit 自动生成的 AI 归因日志 | 这是所有统计的数据源 |
| **Authorship Note 直接解析** | `git notes --ref=ai show <sha>` | 输出该 commit 的原始 attestation（逐文件、逐行范围的 AI/人工归因）+ JSON 元数据（prompt 的 tool/model 信息） | 上传脚本解析它获取**逐文件级**的 AI 归因明细（`Get-CommitAiFileStats`） |
| **推送机制** | `push_authorship_notes()`（`src/git/sync_authorship.rs`） | git push 时自动把 notes 推到远端 | 已有，无需改动 |

**关键发现（影响设计决策）：**
- `git-ai stats <sha> --json` 已经能输出我们需要的所有数据（AI 行数、人工行数、工具分解等），不需要修改 Rust 代码
- 最新安装脚本已经会自动执行 `git-ai install-hooks`；`git-ai git-hooks ensure` 这条旧路径在当前代码里已经 sunset，不能再作为主方案依赖
- 当前 `git-ai config` 只支持固定顶层键以及 `feature_flags.*`、`git_ai_hooks.*` 这两类嵌套键，**不支持** `report_to_remote.*` 这一类自定义上传配置；远程 endpoint / api_key 需要通过环境变量或 Speckit 自己的脚本配置解决
- 对于“某个 commit 有没有 AI authorship note”，不能依赖 `git-ai stats` 是否为空来判断；当前可靠做法是直接查询 `git notes --ref=ai list <sha>`
- 最新 `git-ai` 已内建 `api_base_url` / `api_key` / `personal-dashboard` 这条官方后端路径；如果团队可以接受 Git AI Enterprise 或自托管后端，应优先评估原生路径，本文 3.3-3.5 仅针对“必须对接自有 API”的情况
- commit 时已经在本地写好 authorship note 了，我们只需要在需要的时候"读取 + 上传"
- **逐文件统计使用 commit-local 语义**：直接解析 `git notes --ref=ai show <sha>` 的 attestation 段（非缩进行=文件路径，缩进行=`<id> <range>` 归因条目，`h_*` 前缀=人工，其他=AI prompt hash），结合 `git diff-tree --numstat` 获取每个文件的新增/删除行数，无需调用 `git-ai diff`（后者是 provenance-traced，会跨 commit 追溯，不适合 commit-local 场景）

---

## 二、需求 1 详细实施：安装或更新 Speckit 时安装/更新 git-ai

### 2.1 我们要达到什么效果？

**目标：**
- 新成员执行 `specify init . --ai copilot` 后，能立刻补齐 git-ai 安装和 hooks 配置。
- 已有项目执行 `specify init --here --force --ai copilot --script ps` 更新 `.specify/` 时，能顺带更新 git-ai。

**为什么不让成员自己装？**  
因为实际情况是：你发一个安装文档给 10 个人，最终只有 3 个人会照做。自动化安装才能保证团队覆盖率。

### 2.2 技术方案选择——为什么这样做？

`specify init` 本身来自外部 `specify-cli`，当前仓库并不包含它的实现代码。与此同时，Speckit 也没有标准的 `postInit` 生命周期钩子。因此要分清两类场景：
- **可控链路：** 当前仓库自己的包装脚本，例如 `batch-update.ps1`
- **不可控链路：** 用户手工执行的裸命令 `specify init . --ai copilot`

所以我们考虑了 4 种方案：

| 方案 | 做法 | 优点 | 缺点 | 结论 |
|------|------|------|------|------|
| A. 修改 Agent prompt | 在 `speckit.specify.agent.md` 里加一步"先执行 post-init.ps1" | 改动小，适合补充 Agent 流程体验 | 只在用户通过 Agent 走流程时触发，不覆盖 CLI 初始化 | ⚪ **可选兜底** |
| B. 改 check-prerequisites | 在 `check-prerequisites.ps1` 开头检测 git-ai | 后续任何 Speckit 流程都能提醒 | 不是初始化时立即执行 | ✅ 保留兜底 |
| C. 在包装脚本里串联 post-init | `specify init ...` 成功后立刻执行 `.specify/scripts/powershell/post-init.ps1` | 能覆盖我们自己控制的更新链路；不改 git-ai Rust 代码 | 覆盖不到用户直接手敲的裸 `specify init`，除非用户也走包装脚本 | ✅ **本仓库立即可落地** |
| D. 改 `specify-cli` 上游 | 给 `specify init` 增加 post-init hook | 唯一能真正覆盖裸命令的方案 | 需要同步到 `specify-cli` 发布源，不在当前仓库内 | ⏳ 需要上游配合 |

**最终决策：C + A + B。**
- 对 `specify init --here --force --ai copilot --script ps` 这类我们可控的更新命令，直接在包装脚本后追加 `post-init.ps1 -ForceInstall`。
- 对 Agent 流程，继续保留 Step 0 / `check-prerequisites` 兜底。
- 对裸 `specify init . --ai copilot`，当前仓库只能提供 `post-init.ps1` 作为紧随其后的补充步骤；若要完全自动化，必须改 `specify-cli` 上游。

### 2.2.1 先记住：这次真实要改的不是 1 个文件，而是一组文件

如果只是为了“让生成出来的项目里有 `.specify/scripts/powershell/post-init.ps1`”，很容易误以为只改 `.specify/` 目录就够了。**这是最容易踩的坑。**

在 Spec Kit 源码仓库里，当前这套能力真正落地时，需要同时改下面这几类文件：

| 文件 | 作用 | 为什么必须改它 |
|------|------|---------------|
| `spec-kit/src/specify_cli/__init__.py` | `specify init` 入口 | 负责在初始化完成后自动执行 post-init |
| `spec-kit/scripts/powershell/post-init.ps1` | PowerShell 模板源文件 | Windows 下生成项目时真正会被带出去的脚本源 |
| `spec-kit/scripts/bash/post-init.sh` | Bash 模板源文件 | `--script sh` 时真正会被带出去的脚本源 |
| `spec-kit/.specify/scripts/powershell/post-init.ps1` | 仓库内自举副本 | 方便在 Spec Kit 自己这个仓库里直接验证 PowerShell 流程 |
| `spec-kit/test-verify/.specify/scripts/powershell/post-init.ps1` | 验证副本 | 保证仓库内的验证目录和真实模板行为一致 |

**注意：** 当前仓库里的 `.specify/` 和 `test-verify/.specify/` 目录只维护 PowerShell 副本；bash 路径目前以 `spec-kit/scripts/bash/post-init.sh` 这份模板源为准。

**一句话记忆：**

- `scripts/` 目录是“模板源文件”
- `.specify/` 和 `test-verify/.specify/` 是“仓库内副本/验证副本”
- `src/specify_cli/__init__.py` 是“触发入口”

### 2.2.2 不要改错位置：为什么 `scripts/` 才是模板源

这一步一定要讲透，否则别人照着做时最容易只改错地方。

当前 Spec Kit 的实际行为是这样的：

1. **在线初始化路径**：`specify init` 从 GitHub release ZIP 解压模板
2. **离线初始化路径**：`specify init --offline` 走 wheel 里的 `core_pack`
3. **离线打包来源**：`core_pack/scripts/powershell` 和 `core_pack/scripts/bash` 都是从仓库根下的 `scripts/` 目录打包进去的

所以，**真正的模板源文件是 `spec-kit/scripts/powershell/*.ps1` 和 `spec-kit/scripts/bash/*.sh`**。

这意味着：

- 你如果只改 `spec-kit/.specify/scripts/powershell/post-init.ps1`，仓库里看起来有文件了，但 `specify init` 生成新项目时不一定会带上这份改动
- 你如果只改 `scripts/`，通常已经足够影响模板生成；但为了让仓库自己的验证目录和开发态体验保持一致，仍然应该同步 `.specify/` 和 `test-verify/.specify/` 的副本
- `pyproject.toml` 在当前实现里**不用改**，因为 `scripts/powershell` 和 `scripts/bash` 早就已经被 `force-include` 到 wheel 的 `core_pack` 里了

### 2.3 具体要做的事情（逐步操作）

#### 2.3.0 建议按这个顺序动手，不容易漏

如果要让当前仓库里的方案真正落地，推荐按下面顺序执行：

1. 先准备 `post-init.ps1`，把“检测 → 安装/更新 → 刷新 install-hooks”逻辑单独封装出来
2. 再把 `.specify/scripts/powershell/batch-update.ps1` 接到 `specify init --here --force --ai copilot --script ps` 成功后的 post-init 调用
3. 然后在 Agent prompt 里补一个显式的 Step 0，确保 Agent 流程也会先跑 post-init
4. 最后再补 `check-prerequisites.ps1` 的兜底提示

为什么这个顺序更稳：

- 先有 `post-init` 脚本，再让包装链路和 Agent 去触发它，避免触发点先落地、脚本本身还不存在
- 先打通可控自动化链路，再补 Agent / 前置检查，能优先验证主路径是否真实生效
- `check-prerequisites` 本身只是提醒，不该反过来驱动主流程

#### 第 1 步：创建 `post-init` 脚本（PowerShell 为主，bash 同步补齐）

**要做什么：**

- 在 `.specify/scripts/powershell/` 目录下新建 `post-init.ps1`
- 如果要支持 `specify init --script sh`，同时在 `.specify/scripts/bash/` 下新建 `post-init.sh`

**为什么：** 这个脚本封装了"检测 → 安装/更新 → 刷新 install-hooks"的完整逻辑。它可以被 Agent prompt 调用，也可以被 `batch-update.ps1` 或手工命令在 `specify init` 之后立即执行。这样做能保持职责清晰：

- `specify init` 负责“何时调用 post-init”
- `post-init.ps1` 负责“git-ai 到底怎么安装、怎么刷新 install-hooks”

另外，当前已经验证过的本地原型**不依赖目标项目仓库根目录里恰好存在 `install.ps1/install.sh`**。它默认从 git-ai 官方安装入口下载安装器，并允许团队通过环境变量 `GIT_AI_INSTALLER_URL` 覆盖安装地址。这样更稳，因为 Spec Kit 会运行在任意项目里，不能假设项目根目录就是 git-ai 仓库。

**文件路径：**

- PowerShell: `.specify/scripts/powershell/post-init.ps1`
- Bash: `.specify/scripts/bash/post-init.sh`

**如果你当前是在 Spec Kit 源码仓库里改代码，这里要转换成真实落点：**

- 先改：`spec-kit/scripts/powershell/post-init.ps1`
- 再改：`spec-kit/scripts/bash/post-init.sh`
- 再同步：`spec-kit/.specify/scripts/powershell/post-init.ps1`
- 再同步：`spec-kit/test-verify/.specify/scripts/powershell/post-init.ps1`

也就是说，文档里写的 `.specify/scripts/...` 是“生成后的项目里会出现的路径”；而你在 Spec Kit upstream 仓库里真正要编辑的主文件，是 `scripts/...` 目录下那两份源脚本。

**实现文件（以仓库中的真实脚本为准）：**

- `.specify/scripts/powershell/post-init.ps1`
- 下方代码块只作为设计说明，真实行为以仓库里的脚本实现为准

**示意代码：**

```powershell
#!/usr/bin/env pwsh

[CmdletBinding()]
param(
    [switch]$ForceInstall,
    [switch]$Skip
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot/common.ps1"

$GitAiInstallScriptUrl = if ($env:GIT_AI_INSTALLER_URL) {
    $env:GIT_AI_INSTALLER_URL
} else {
    'https://usegitai.com/install.ps1'
}
$GitAiExecutablePath = Join-Path $HOME '.git-ai\bin\git-ai.exe'

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
} catch { }

function Write-PostInitInfo {
    param([string]$Message)
    Write-Host "[speckit/post-init] $Message" -ForegroundColor Cyan
}

function Write-PostInitSuccess {
    param([string]$Message)
    Write-Host "[speckit/post-init] $Message" -ForegroundColor Green
}

function Write-PostInitWarning {
    param([string]$Message)
    Write-Warning "[speckit/post-init] $Message"
}

function Get-GitAiCommand {
    $command = Get-Command git-ai -ErrorAction SilentlyContinue
    if ($command -and $command.Path) {
        return $command.Path
    }

    if (Test-Path -LiteralPath $GitAiExecutablePath) {
        return $GitAiExecutablePath
    }

    return $null
}

function Invoke-GitAiInstaller {
    $tempInstaller = Join-Path ([System.IO.Path]::GetTempPath()) ("git-ai-install-{0}.ps1" -f [System.Guid]::NewGuid().ToString('N'))

    try {
        Write-PostInitInfo "Downloading git-ai installer from GitHub..."
        Invoke-WebRequest -Uri $GitAiInstallScriptUrl -OutFile $tempInstaller
        & $tempInstaller
    } finally {
        Remove-Item -LiteralPath $tempInstaller -ErrorAction SilentlyContinue
    }
}

function Refresh-GitAiInstallHooks {
    $gitAiCommand = Get-GitAiCommand

    if (-not $gitAiCommand) {
        Write-PostInitWarning "git-ai is not available in this shell. The installer already ran install-hooks; if needed, run 'git-ai install-hooks' manually after your PATH is refreshed."
        return
    }

    try {
        Write-PostInitInfo 'Refreshing git-ai install-hooks configuration...'
        & $gitAiCommand install-hooks | Out-Host
        if ($LASTEXITCODE -eq 0) {
            Write-PostInitSuccess 'git-ai install-hooks completed successfully.'
        } else {
            Write-PostInitWarning "git-ai install-hooks exited with code $LASTEXITCODE. Run it manually if the integration was not refreshed."
        }
    } catch {
        Write-PostInitWarning "install-hooks refresh failed: $_"
    }
}

if ($Skip) {
    Write-PostInitInfo 'Skipping git-ai setup because -Skip was provided.'
    exit 0
}

$existingCommand = Get-GitAiCommand
if ($existingCommand -and -not $ForceInstall) {
    $version = & $existingCommand --version 2>$null
    if ($version) {
        Write-PostInitSuccess "git-ai already installed: $version"
    } else {
        Write-PostInitSuccess 'git-ai already installed.'
    }
} else {
    try {
        Invoke-GitAiInstaller
    } catch {
        Write-PostInitWarning "git-ai installation failed: $_"
        Write-PostInitWarning 'You can rerun this script later without blocking Spec Kit initialization.'
        exit 0
    }

    $installedCommand = Get-GitAiCommand
    if ($installedCommand) {
        $version = & $installedCommand --version 2>$null
        if ($version) {
            Write-PostInitSuccess "git-ai installed successfully: $version"
        } else {
            Write-PostInitSuccess 'git-ai installed successfully.'
        }
    } else {
        Write-PostInitWarning 'git-ai installer completed, but the command is not yet available in this shell. The default install path will still be used if present.'
    }
}

Refresh-GitAiInstallHooks

Write-PostInitSuccess 'git-ai post-init completed.'
Write-Host '[speckit/post-init] Future git commits in this repository will record AI authorship data when git-ai is available.'
```

**补充说明：** 当前真实实现里，bash 路径的模板源文件是 `spec-kit/scripts/bash/post-init.sh`；仓库内 `.specify/` 自举目录目前仍然只维护 PowerShell 脚本。无论 PowerShell 还是 bash，最终目标行为都一致：都是“检测已有安装 → 调官方安装器 → 刷新 install-hooks → 失败只 warning”。

**这里再强调一次职责边界：**

- `post-init.ps1` / `post-init.sh` 只负责 git-ai 的安装与 hooks 配置
- `specify_cli/__init__.py` 只负责“什么时候执行 post-init”
- 远程统计 endpoint / api_key 不属于当前默认安装逻辑，保持为后续可选增强

**验证方法：** 创建完脚本后，在项目根目录执行以下命令验证：

```powershell
# 测试脚本是否能正常运行
.\.specify\scripts\powershell\post-init.ps1

# 测试更新路径（强制重装/更新 git-ai）
.\.specify\scripts\powershell\post-init.ps1 -ForceInstall

# 预期输出（已安装 git-ai 的情况）：
# [speckit/post-init] git-ai 已安装: git-ai x.x.x
# [speckit/post-init] git-ai 已安装，跳过安装步骤
# [speckit/post-init] 在仓库中配置 git-ai hooks: C:\Users\xxx\project
# [speckit/post-init] ✓ git-ai hooks 配置成功
# [speckit/post-init] ✓ git-ai 集成配置完成！
```

---

#### 第 2 步：在可控的更新链路后自动触发 `post-init.ps1`

**要做什么：** 修改仓库内负责批量刷新 `.specify/` 的脚本，在 `specify init --here --force --ai copilot --script ps` 成功后立即执行 `.specify/scripts/powershell/post-init.ps1 -ForceInstall`。

**为什么：**

- 这条链路是当前仓库能直接控制的“更新 spec”入口。
- `.specify/` 被 `--force` 覆盖时，顺手更新 git-ai 才符合“更新 spec 时同步更新 git-ai”的目标。
- 不需要改 `specify-cli` 上游，也不需要改 git-ai Rust 代码。

**注意边界：** 这一步只覆盖我们自己的包装脚本，**不能自动劫持用户手工输入的裸命令** `specify init . --ai copilot`。后者如果要自动触发，必须由 `specify-cli` 官方实现 `post-init` hook，或要求团队改用包装命令。

**实现点：**

- 文件：`.specify/scripts/powershell/batch-update.ps1`
- 位置：`specify init --here --force --ai copilot --script ps` 成功返回后
- 调用：`& .specify/scripts/powershell/post-init.ps1 -ForceInstall`

**验证方法：** 对一个已初始化项目执行批量更新，确认日志里先出现 `Successfully ran specify init --here --force --ai copilot --script ps`，随后出现 `[speckit/post-init]` 的 git-ai 更新日志。

---

#### 第 3 步：修改 Agent prompt，使 Speckit 流程自动触发安装

**要做什么：** 在 `speckit.specify.agent.md` 的最前面增加一个显式的 Step 0，优先执行 `pwsh .specify/scripts/powershell/post-init.ps1`，再继续原有的 Spec / Plan / Tasks 流程。

**为什么：**

- Agent 流程是团队最常用的可控入口之一。
- 即使没有走 `batch-update.ps1`，只要成员通过 Agent 进入 Speckit 流程，也能先补齐 git-ai。
- 这一步和第 2 步是互补关系，不是替代关系。

**核心原则：**

- post-init 失败只输出 warning，不中断 Speckit 主流程。
- Agent 只负责编排，不复制 git-ai 的安装逻辑。
- Agent 文案里要明确告诉用户：裸 `specify init . --ai copilot` 仍需命令后补跑 `post-init.ps1`。

**验证方法：** 触发一次 Agent 驱动的 Speckit 流程，确认最前面出现 `post-init.ps1` 执行日志，然后再进入后续步骤。

---

#### 第 4 步（可选兜底）：在 check-prerequisites 中加入 git-ai 检测

**要做什么：** 在 `.specify/scripts/powershell/check-prerequisites.ps1` 的前置检查中增加 git-ai 安装状态检测。

**为什么：** 这一步不再是主触发机制，而是给**存量项目**做兜底。比如：

- 某个项目是通过裸 `specify init . --ai copilot` 初始化的，但没有在命令后补跑 `post-init.ps1`
- 某个成员直接复制了 `.specify/` 目录，没有重新执行 `specify init`
- 某个旧仓库只跑了部分脚本，没有触发 post-init

这种情况下，`check-prerequisites.ps1` 至少能给出明确提醒，告诉用户缺的是 git-ai。

**具体怎么改：** 打开 `.specify/scripts/powershell/check-prerequisites.ps1`，在 `. "$PSScriptRoot/common.ps1"` 这一行之后、`$paths = Get-FeaturePathsEnv` 之前，插入以下代码：

```powershell
# ── git-ai 安装检测（兜底） ──
# 为什么加在这里：即使用户没走 Agent 流程，只要执行任何 speckit 前置检查都会触发
$gitAiCmd = Get-Command git-ai -ErrorAction SilentlyContinue
if (-not $gitAiCmd) {
    Write-Warning ""
    Write-Warning "╔══════════════════════════════════════════════════════╗"
    Write-Warning "║  [speckit] git-ai 未检测到！                        ║"
    Write-Warning "║  AI 代码归因功能将不可用。                           ║"
    Write-Warning "║                                                      ║"
    Write-Warning "║  安装方法：                                          ║"
    Write-Warning "║  .\.specify\scripts\powershell\post-init.ps1         ║"
    Write-Warning "╚══════════════════════════════════════════════════════╝"
    Write-Warning ""
    # 注意：只警告，不阻塞。git-ai 不是 Speckit 的硬依赖
}
```

**为什么只警告不阻塞？** 因为 git-ai 是「增强功能」而非「必需功能」。即使没有 git-ai，Speckit 的 spec → plan → tasks → code-review 流程本身完全能正常工作。我们不应该因为一个增强工具没装就卡住团队成员的开发流程。

---

## 三、需求 2 详细实施：AI 检测结果上传到远程

### 3.1 先搞清楚：为什么不在每次 commit 时自动上传？

你可能会想："既然 git-ai 在每次 commit 时已经自动记录了 AI 归因数据，为什么不直接在 commit 的时候就上传到远程？"

**原因：**
1. **commit 速度敏感**：开发者一天可能 commit 几十次，每次加一个网络请求（哪怕异步的）都会拖慢体验。网络差的时候更痛苦。
2. **高频低价值**：开发过程中的中间 commit（比如 "wip: save progress"）的统计数据对团队管理没有价值，leader 只关心最终合入主分支的代码。
3. **隐私顾虑**：自动静默上传会让开发者觉得被"监控"，心理抵触。主动上传让开发者有控制感。

**所以我们选择了两条上传路径：**

| 路径 | 谁触发 | 什么时候 | 为什么 |
|------|--------|---------|--------|
| **A. 主动上传命令** | 开发者自己 | 开发完一个功能分支、准备提 MR 之前 | 开发者自己决定什么时候上传，有控制权 |
| **B. Code Review 自动上传** | Code Review Agent | leader 发起 code review 时 | 审查天然需要看代码质量，AI 使用统计是审查的一部分 |

**两条路径互补：** 如果开发者主动上传了，Code Review 时也会再次确认数据完整性（API 做 upsert，重复上传不会重复记录）。如果开发者忘了上传，Code Review 兜底。

### 3.2 整体数据流（一张图看懂全链路）

```
  开发者写代码（用 AI 工具辅助）
       │
       ▼
  git commit（本地触发 git-ai hooks）
       │
       ▼
  ┌──────────────────────────────┐
  │ git-ai 自动记录（本地操作）    │  ← 这一步每次 commit 都自动发生
  │                              │
  │ 做了什么：                    │
  │ 1. 分析 diff，识别哪些行是     │
    │    AI 归因、哪些发生了混编     │
  │ 2. 结果写入 Git Note          │
  │    (refs/notes/ai)           │
  │ 3. 不联网，不上传，零开销      │
  └──────────┬───────────────────┘
             │
             │ 数据留在本地，等待以下任一路径触发上传：
             │
    ┌────────┴─────────────────────────┐
    │                                  │
    ▼                                  ▼
 ┌───────────────────┐     ┌───────────────────────┐
 │ 路径 A：主动上传    │     │ 路径 B：Code Review    │
 │                    │     │ 自动上传               │
 │ 触发方式：          │     │                       │
 │ 开发者手动执行      │     │ 触发方式：              │
 │ upload-ai-stats.ps1│     │ leader 发起 code review│
 │                    │     │ Agent 步骤 8.3/8.4     │
 │ 数据来源：          │     │ 自动调用               │
 │ git-ai stats <sha> │     │                       │
 │ --json             │     │ 数据来源：同左          │
 └────────┬───────────┘     └──────────┬────────────┘
          │                            │
          ▼                            ▼
 ┌──────────────────────────────────────────────┐
 │ 远程 API: 由接入方显式提供完整 URL           │
 │ 或 base endpoint + path 组合                 │
 │                                              │
 │ 客户端一次发送一个批量请求（commits[]）      │
│ 服务端按 commitSha 做幂等去重               │
 │ 返回 results[] 标明每个 commit 的状态        │
 │ 通过 source 字段区分来源                     │
 │ ("manual" 或 "code-review")                 │
 └──────────────────────────────────────────────┘
```

### 3.3 路径 A 详细实施：`upload-ai-stats.ps1` 主动上传脚本

> **基于 2026-04-16 最新 `git-ai` 的建议：** 如果团队可以直接使用 Git AI Enterprise 或自托管 Git AI 后端，优先评估原生路径（`GIT_AI_API_KEY` / `GIT_AI_API_BASE_URL` + `git-ai personal-dashboard`）。下面这套 `upload-ai-stats.ps1` + 自建上传接口的方案，仅适用于“必须把数据发往自有接口”的情况。

#### 第 1 步：创建 `upload-ai-stats.ps1` 脚本

**要做什么：** 在 `.specify/scripts/powershell/` 目录下新建 `upload-ai-stats.ps1`。

**为什么单独做一个脚本（而不是在 git-ai Rust 代码里加）？**
- git-ai 是一个通用工具，不应该预设"上传到某个特定 API"
- 我们团队的远程 API 地址、认证方式、数据格式可能随时调整，PowerShell 脚本改起来比 Rust 快
- Speckit 的其他脚本也是 PowerShell，保持一致性

**文件路径：** `.specify/scripts/powershell/upload-ai-stats.ps1`

**使用方法（4 种场景）：**

```powershell
# 场景 1：上传当前分支所有 commit 的 AI 统计（最常用）
# 什么时候用：功能开发完，准备提 MR 之前
.\.specify\scripts\powershell\upload-ai-stats.ps1

# 场景 2：上传指定日期范围的 commit
# 什么时候用：月底/周报需要统计一段时间的数据
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Since "2026-04-01" -Until "2026-04-14"

# 场景 3：上传指定的几个 commit
# 什么时候用：只想上传特定的 commit，比如修复了一个 bug
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits "abc123,def456"

# 场景 4：先预览不上传（dry run）
# 什么时候用：不确定会上传什么数据，先看看
.\.specify\scripts\powershell\upload-ai-stats.ps1 -DryRun
```

**完整脚本代码（可直接复制使用）：**

```powershell
#!/usr/bin/env pwsh
<#
.SYNOPSIS
    主动上传 git-ai 检测到的 AI 代码使用统计到远程 API。
.DESCRIPTION
    该脚本完成以下工作：
    1. 收集目标 commit（默认是当前分支相对 main 的所有 commit）
    2. 对每个 commit 调用 git-ai stats <sha> --json 获取 AI 使用统计
    3. 将统计数据 POST 到远程 API
    
    不修改任何 git 数据，纯读取 + 上传。
.EXAMPLE
    # 上传当前分支所有 commit
    .\.specify\scripts\powershell\upload-ai-stats.ps1
    
    # 预览不上传
    .\.specify\scripts\powershell\upload-ai-stats.ps1 -DryRun
    
    # 上传指定 commit
    .\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits "abc123,def456"
#>
[CmdletBinding()]
param(
    [string]$Since,          # 开始日期 YYYY-MM-DD
    [string]$Until,          # 结束日期 YYYY-MM-DD
    [string]$Commits,        # 逗号分隔的 commit SHA
    [string]$Author,         # 筛选作者（邮箱）
    [string]$Source = "manual",  # 上传来源：manual / codeReview
    [string]$ReviewDocumentId,     # 审查场景关联的文档 ID
    [switch]$Json,           # JSON 输出（供 Agent 调用时解析）
    [switch]$DryRun,         # 只收集和展示，不真正上传
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

# 加载公共函数库（复用 Get-RepoRoot 等）
. "$PSScriptRoot/common.ps1"

# ─── 函数定义 ────────────────────────────────────────────────

function Get-TargetCommits {
    <#
    .SYNOPSIS 根据参数确定要处理哪些 commit
    .DESCRIPTION
        优先级：
        1. 如果传了 -Commits 参数 → 使用指定的 SHA
        2. 如果传了 -Since/-Until → 按日期范围筛选
        3. 都没传 → 默认取当前分支相对 main/master 的所有 commit
        
        为什么默认 "相对 main 的 commit"？
        因为功能分支上的 commit = 本次开发的全部工作量，
        这正是 leader 想看到的数据范围。
    #>
    $repoRoot = Get-RepoRoot
    $gitArgs = @("log", "--format=%H")

    if ($Commits) {
        return $Commits -split ',' | ForEach-Object { $_.Trim() }
    }

    if ($Since) { $gitArgs += "--since=$Since" }
    if ($Until) { $gitArgs += "--until=$Until" }
    if ($Author) { $gitArgs += "--author=$Author" }

    if (-not $Since -and -not $Until) {
        # 找到默认基分支（main 或 master）
        $baseBranch = git -C $repoRoot symbolic-ref refs/remotes/origin/HEAD 2>$null
        if (-not $baseBranch) { $baseBranch = "origin/main" }
        $baseBranch = $baseBranch -replace 'refs/remotes/', ''
        $gitArgs += "$baseBranch..HEAD"
    }

    $result = git -C $repoRoot @gitArgs 2>$null
    if ($LASTEXITCODE -ne 0) { return @() }
    return ($result -split "`n" | Where-Object { $_ })
}

function Join-ProcessArguments {
    param([string[]]$Arguments)

    return ($Arguments | ForEach-Object {
        if ($_ -match '[\s"]') {
            '"{0}"' -f ($_.Replace('"', '\"'))
        } else {
            $_
        }
    }) -join ' '
}

function Invoke-ProcessCapture {
    param(
        [string]$FilePath,
        [string[]]$Arguments
    )

    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $FilePath
    $startInfo.Arguments = Join-ProcessArguments -Arguments $Arguments
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo

    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()

    return @{
        ExitCode = $process.ExitCode
        StdOut = $stdout
        StdErr = $stderr
    }
}

function Get-AuthorshipNoteLookup {
    param([string]$RepoRoot)

    if (-not $script:AuthorshipNoteLookupCache) {
        $script:AuthorshipNoteLookupCache = @{}
    }

    if ($script:AuthorshipNoteLookupCache.ContainsKey($RepoRoot)) {
        return $script:AuthorshipNoteLookupCache[$RepoRoot]
    }

    $lookup = @{}
    $noteLines = git -C $RepoRoot notes --ref=ai list 2>$null
    if ($LASTEXITCODE -eq 0 -and $noteLines) {
        foreach ($line in ($noteLines -split "`n" | Where-Object { $_.Trim() })) {
            $parts = $line.Trim() -split '\s+', 2
            if ($parts.Count -eq 2) {
                $lookup[$parts[1]] = $true
            }
        }
    }

    $script:AuthorshipNoteLookupCache[$RepoRoot] = $lookup
    return $lookup
}

function Test-CommitHasAuthorshipNote {
    param(
        [string]$RepoRoot,
        [string]$CommitSha
    )

    $lookup = Get-AuthorshipNoteLookup -RepoRoot $RepoRoot
    return $lookup.ContainsKey($CommitSha)
}

function Get-CommitAiStats {
    <#
    .SYNOPSIS 调用 git-ai stats 获取单个 commit 的 AI 使用统计（commit 级 + 文件级）
    .DESCRIPTION
                当前可靠流程是：
                1. 无论有没有 authorship note，都调用 git-ai stats <sha> --json 获取 commit 级汇总
                2. 额外用 git notes --ref=ai list <sha> 只做 hasAuthorshipNote 标记
                3. 调用 Get-CommitAiFileStats 解析 authorship note attestation 段，获取逐文件级归因明细
                4. 对于没有 note 的 commit，最新 git-ai 会把多数/全部新增行落到 unknown_additions，而不是返回空

        git-ai stats <sha> --json 会输出类似这样的 JSON:
        {
                    "human_additions": 105,
                    "unknown_additions": 15,
          "ai_additions": 80,
          "ai_accepted": 65,
          "mixed_additions": 15,
          ...
        }

                上传到远程 Java 接口前，脚本会再把这些原始 snake_case 字段转换成 camelCase DTO，
                并把 `tool_model_breakdown` 展开成 `toolModelBreakdown[]`。

        逐文件统计来自 Get-CommitAiFileStats，它直接解析 commit 自身的 authorship note
        （git notes --ref=ai show <sha>），结合 git diff-tree --numstat，产出 stats.files[]。
        这是 commit-local 语义，只看当前 commit 的归因，不做跨 commit 的 provenance 追溯。
    #>
    param([string]$CommitSha)

    $repoRoot = Get-RepoRoot
    $hasAuthorshipNote = Test-CommitHasAuthorshipNote -RepoRoot $repoRoot -CommitSha $CommitSha

    $statsCommandResult = Invoke-ProcessCapture -FilePath 'git-ai' -Arguments @('stats', $CommitSha, '--json')
    $statsJson = $statsCommandResult.StdOut
    if ($statsCommandResult.ExitCode -ne 0 -or -not $statsJson) {
        Write-Warning "[upload-ai-stats] 读取统计失败($($CommitSha.Substring(0,7)))"
        return $null
    }

    $statsObject = $statsJson | ConvertFrom-Json

    # 逐文件统计：解析 authorship note attestation + git diff-tree --numstat
    $fileStats = @(Get-CommitAiFileStats -CommitSha $CommitSha)
    if ($statsObject.PSObject.Properties.Name -contains 'files') {
        $statsObject.files = $fileStats
    } else {
        $statsObject | Add-Member -NotePropertyName 'files' -NotePropertyValue $fileStats
    }

    return @{
        HasAuthorshipNote = $hasAuthorshipNote
        Stats = $statsObject
    }
}

function Get-CommitAiFileStats {
    <#
    .SYNOPSIS 解析 authorship note attestation 段 + git diff-tree --numstat，产出逐文件级归因明细
    .DESCRIPTION
        commit-local 语义：只看当前 commit 自身的 authorship note，不做跨 commit 的 provenance 追溯。

        实现步骤：
        1. git diff-tree --no-commit-id --numstat -r <sha> → 每个文件的 added/deleted 行数
        2. git notes --ref=ai show <sha> → 该 commit 的 authorship note 原文
        3. 解析 attestation 段（"---" 分隔符之前）：
           - 非缩进行 = 文件路径
           - 缩进行 = "<id> <start>-<end>[,<start>-<end>...]"
           - h_* 前缀的 id = 人工归因，其他 = AI prompt hash
        4. 解析 JSON 元数据段（"---" 分隔符之后）：
           - prompts.<hash>.agent_id.tool / .model → 用于 tool_model_breakdown
        5. 合并：numstat 的总行数 + attestation 的 AI/人工行数 → 每个文件的 stats 对象

        为什么不用 git-ai diff？
        - git-ai diff 是 provenance-traced，会跨 commit 追溯行的来源
        - 例如 commit A 是纯人工，但其中某些行最初来自更早的 AI commit，git-ai diff 会把它们归为 AI
        - 这不符合 commit-local 的业务语义（"这个 commit 本身有多少 AI 参与"）
        - 直接解析 authorship note attestation 段则完全是 commit-local 的
    #>
    param([string]$CommitSha)

    $repoRoot = Get-RepoRoot

    # Step 1: 每个文件的 added/deleted 行数
    $numstatResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'diff-tree', '--no-commit-id', '--numstat', '-r', $CommitSha)
    if ($numstatResult.ExitCode -ne 0 -or -not $numstatResult.StdOut) { return @() }

    $fileLineCounts = [ordered]@{}
    foreach ($numLine in ($numstatResult.StdOut -split "`n")) {
        $numLine = $numLine.Trim()
        if (-not $numLine) { continue }
        $parts = $numLine -split "`t", 3
        if ($parts.Count -lt 3) { continue }
        $added   = if ($parts[0] -eq '-') { 0 } else { [int]$parts[0] }
        $deleted = if ($parts[1] -eq '-') { 0 } else { [int]$parts[1] }
        $fileLineCounts[$parts[2]] = @{ added = $added; deleted = $deleted }
    }
    if ($fileLineCounts.Count -eq 0) { return @() }

    # Step 2: 读取 authorship note（commit-local）
    $noteResult = Invoke-ProcessCapture -FilePath 'git' -Arguments @('-C', $repoRoot, 'notes', '--ref=ai', 'show', $CommitSha)
    $fileAttestations = @{}; $promptsMetadata = @{}

    if ($noteResult.ExitCode -eq 0 -and $noteResult.StdOut) {
        # 分割 attestation 段 / JSON 元数据段（以 "---" 为界）
        $sepMatch = [regex]::Match($noteResult.StdOut, '(?m)^---\s*$')
        $attestationText = ''; $jsonText = ''
        if ($sepMatch.Success) {
            $attestationText = $noteResult.StdOut.Substring(0, $sepMatch.Index)
            $jsonText = $noteResult.StdOut.Substring($sepMatch.Index + $sepMatch.Length)
        }

        # 解析 JSON 元数据 → prompt tool/model
        if ($jsonText) {
            try {
                $metadata = $jsonText.Trim() | ConvertFrom-Json
                $prompts = Get-ResponsePropertyValue -Object $metadata -Names @('prompts')
                if ($prompts) {
                    foreach ($pe in (Get-ObjectEntries -Object $prompts)) {
                        $promptsMetadata[[string]$pe.Name] = $pe.Value
                    }
                }
            } catch { }
        }

        # 解析 attestation 段：非缩进行=文件路径，缩进行="<id> <range>"
        $currentFile = $null
        foreach ($attLine in ($attestationText -split "`n")) {
            if ([string]::IsNullOrWhiteSpace($attLine)) { continue }
            if ($attLine -match '^\S') {
                $currentFile = $attLine.Trim()
                if (-not $fileAttestations.ContainsKey($currentFile)) {
                    $fileAttestations[$currentFile] = @{ ai = 0; human = 0; tool_model_breakdown = @{} }
                }
                continue
            }
            if (-not $currentFile -or $attLine -notmatch '^\s+(\S+)\s+(.+)$') { continue }
            $entryId = $Matches[1]; $rangeStr = $Matches[2]; $lineCount = 0
            foreach ($rp in ($rangeStr -split ',')) {
                $rp = $rp.Trim()
                if ($rp -match '^(\d+)-(\d+)$') { $lineCount += [int]$Matches[2] - [int]$Matches[1] + 1 }
                elseif ($rp -match '^\d+$') { $lineCount += 1 }
            }
            if ($lineCount -le 0) { continue }
            if (-not $fileAttestations.ContainsKey($currentFile)) {
                $fileAttestations[$currentFile] = @{ ai = 0; human = 0; tool_model_breakdown = @{} }
            }
            if ($entryId -like 'h_*') {
                $fileAttestations[$currentFile]['human'] += $lineCount
            } else {
                $fileAttestations[$currentFile]['ai'] += $lineCount
                # tool_model_breakdown from prompt metadata
                $tool = 'unknown'; $model = $null
                if ($promptsMetadata.ContainsKey($entryId)) {
                    $agentId = Get-ResponsePropertyValue -Object $promptsMetadata[$entryId] -Names @('agent_id')
                    if ($agentId) {
                        $toolVal  = Get-ResponsePropertyValue -Object $agentId -Names @('tool')
                        $modelVal = Get-ResponsePropertyValue -Object $agentId -Names @('model')
                        if ($toolVal) { $tool = [string]$toolVal }
                        if ($modelVal) { $model = [string]$modelVal }
                    }
                }
                $bkKey = if ([string]::IsNullOrWhiteSpace($model)) { $tool } else { '{0}::{1}' -f $tool, $model }
                if (-not $fileAttestations[$currentFile]['tool_model_breakdown'].ContainsKey($bkKey)) {
                    $fileAttestations[$currentFile]['tool_model_breakdown'][$bkKey] = @{ ai_additions = 0 }
                }
                $fileAttestations[$currentFile]['tool_model_breakdown'][$bkKey]['ai_additions'] += $lineCount
            }
        }
    }

    # Step 3: 合并 numstat + attestation → 逐文件 stats
    $results = @()
    foreach ($filePath in $fileLineCounts.Keys) {
        $lc  = $fileLineCounts[$filePath]
        $att = if ($fileAttestations.ContainsKey($filePath)) { $fileAttestations[$filePath] } else { $null }
        $aiAdd    = if ($att) { [Math]::Min([int]$att['ai'], $lc.added) } else { 0 }
        $humanAdd = if ($att) { [Math]::Min([int]$att['human'], [Math]::Max(0, $lc.added - $aiAdd)) } else { $lc.added }
        $unknown  = [Math]::Max(0, $lc.added - $aiAdd - $humanAdd)
        $results += [pscustomobject]@{
            file_path              = $filePath
            git_diff_added_lines   = $lc.added
            git_diff_deleted_lines = $lc.deleted
            ai_additions           = $aiAdd
            human_additions        = $humanAdd
            unknown_additions      = $unknown
            tool_model_breakdown   = if ($att) { $att['tool_model_breakdown'] } else { @{} }
        }
    }
    return $results
}

function Get-UploadRemoteConfig {
    <#
    .SYNOPSIS 读取远程上传配置
    .DESCRIPTION
        当前方案不依赖 git-ai config，因为 git-ai 现有配置结构不支持 report_to_remote.*。
        上传地址 / api_key 统一通过环境变量注入，这样不用改 git-ai Rust 代码也能落地。
    #>

    $url = $env:GIT_AI_REPORT_REMOTE_URL
    if ($url) {
        return @{
            Url = $url
            ApiKey = $env:GIT_AI_REPORT_REMOTE_API_KEY
                UserId = $env:GIT_AI_REPORT_REMOTE_USER_ID
        }
    }

    $endpoint = $env:GIT_AI_REPORT_REMOTE_ENDPOINT
    $path = $env:GIT_AI_REPORT_REMOTE_PATH
    if (-not $endpoint -or -not $path) {
        Write-Warning "[upload-ai-stats] 请配置 GIT_AI_REPORT_REMOTE_URL，或同时配置 GIT_AI_REPORT_REMOTE_ENDPOINT 与 GIT_AI_REPORT_REMOTE_PATH"
        return $null
    }

    return @{
        Url = "{0}/{1}" -f $endpoint.TrimEnd('/'), $path.TrimStart('/')
        ApiKey = $env:GIT_AI_REPORT_REMOTE_API_KEY
            UserId = $env:GIT_AI_REPORT_REMOTE_USER_ID
    }
}

function Get-ResponsePropertyValue {
    param(
        [object]$Object,
        [string[]]$Names
    )

    if (-not $Object) {
        return $null
    }

    if ($Object -is [System.Collections.IDictionary]) {
        foreach ($name in $Names) {
            if ($Object.Contains($name)) {
                return $Object[$name]
            }
        }

        return $null
    }

    foreach ($name in $Names) {
        if ($Object.PSObject.Properties.Name -contains $name) {
            return $Object.$name
        }
    }

    return $null
}

function Convert-SnakeCaseNameToCamelCase {
    param([string]$Name)

    if ([string]::IsNullOrWhiteSpace($Name) -or $Name -notmatch '_') {
        return $Name
    }

    $segments = $Name -split '_'
    if ($segments.Count -eq 0) {
        return $Name
    }

    $camelName = $segments[0]
    for ($i = 1; $i -lt $segments.Count; $i++) {
        if ([string]::IsNullOrEmpty($segments[$i])) {
            continue
        }

        $camelName += $segments[$i].Substring(0, 1).ToUpperInvariant() + $segments[$i].Substring(1)
    }

    return $camelName
}

function Get-ObjectEntries {
    param([object]$Object)

    if ($null -eq $Object) {
        return @()
    }

    if ($Object -is [System.Collections.IDictionary]) {
        return @($Object.GetEnumerator() | ForEach-Object {
            [pscustomobject]@{
                Name = [string]$_.Key
                Value = $_.Value
            }
        })
    }

    return @($Object.PSObject.Properties | Where-Object {
        $_.MemberType -in @('NoteProperty', 'Property', 'AliasProperty', 'ScriptProperty')
    } | ForEach-Object {
        [pscustomobject]@{
            Name = [string]$_.Name
            Value = $_.Value
        }
    })
}

function Get-NormalizedUploadSource {
    param([string]$Value)

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return 'manual'
    }

    switch ($Value.ToLowerInvariant()) {
        'manual' { return 'manual' }
        'code-review' { return 'codeReview' }
        'code_review' { return 'codeReview' }
        'codereview' { return 'codeReview' }
        default { return $Value }
    }
}

function Convert-ToolModelBreakdownToDto {
    param([object]$Breakdown)

    if ($null -eq $Breakdown) {
        return @()
    }

    $items = @()
    foreach ($entry in (Get-ObjectEntries -Object $Breakdown)) {
        $entryName = [string]$entry.Name
        $tool = $entryName
        $model = $null

        $nameParts = $entryName -split '::', 2
        if ($nameParts.Count -eq 2) {
            $tool = $nameParts[0]
            $model = $nameParts[1]
        }

        $dtoItem = [ordered]@{
            tool = $tool
            model = $model
        }

        $convertedMetrics = Convert-ObjectKeysToCamelCase -Value $entry.Value
        foreach ($metricEntry in (Get-ObjectEntries -Object $convertedMetrics)) {
            $dtoItem[[string]$metricEntry.Name] = $metricEntry.Value
        }

        $items += [pscustomobject]$dtoItem
    }

    return @($items)
}

function Convert-ObjectKeysToCamelCase {
    param([object]$Value)

    if ($null -eq $Value) {
        return $null
    }

    if ($Value -is [string] -or $Value -is [ValueType]) {
        return $Value
    }

    if ($Value -is [System.Array]) {
        return @($Value | ForEach-Object { Convert-ObjectKeysToCamelCase -Value $_ })
    }

    $entries = Get-ObjectEntries -Object $Value
    if ($entries.Count -eq 0) {
        return $Value
    }

    $convertedObject = [ordered]@{}
    foreach ($entry in $entries) {
        $propertyName = [string]$entry.Name
        if ($propertyName -in @('tool_model_breakdown', 'toolModelBreakdown')) {
            $convertedObject['toolModelBreakdown'] = @(Convert-ToolModelBreakdownToDto -Breakdown $entry.Value)
            continue
        }

        $convertedName = Convert-SnakeCaseNameToCamelCase -Name $propertyName
        $convertedObject[$convertedName] = Convert-ObjectKeysToCamelCase -Value $entry.Value
    }

    return [pscustomobject]$convertedObject
}

function Convert-CommitTimestampToUploadFormat {
    param([AllowEmptyString()][string]$Timestamp)

    $trimmedTimestamp = if ($null -eq $Timestamp) { '' } else { $Timestamp.Trim() }
    if (-not $trimmedTimestamp) {
        return ''
    }

    $parsedTimestamp = [System.DateTimeOffset]::MinValue
    $parseSucceeded = [System.DateTimeOffset]::TryParse(
        $trimmedTimestamp,
        [System.Globalization.CultureInfo]::InvariantCulture,
        [System.Globalization.DateTimeStyles]::RoundtripKind,
        [ref]$parsedTimestamp
    )

    if ($parseSucceeded) {
        return $parsedTimestamp.ToString('yyyy-MM-dd HH:mm:ss', [System.Globalization.CultureInfo]::InvariantCulture)
    }

    return $trimmedTimestamp
}

function New-CommitUploadItem {
    <#
    .SYNOPSIS 组装单个 commit 在批量请求中的上传对象
    #>
    param(
        [string]$CommitSha,
        [hashtable]$StatsResult
    )

    $repoRoot = Get-RepoRoot
    $commitInfo = git -C $repoRoot log -1 --format="%ae|%s|%aI" $CommitSha 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $commitInfo) {
        Write-Warning "[upload-ai-stats] 读取 commit 元数据失败($($CommitSha.Substring(0,7)))"
        return $null
    }

    $parts = $commitInfo -split '\|', 3
    $formattedTimestamp = if ($parts.Count -ge 3) { Convert-CommitTimestampToUploadFormat -Timestamp $parts[2] } else { "" }
    return @{
        commitSha = $CommitSha
        commitMessage = if ($parts.Count -ge 2) { $parts[1] } else { "" }
        author = if ($parts.Count -ge 1) { $parts[0] } else { "" }
        timestamp = $formattedTimestamp
        hasAuthorshipNote = [bool]$StatsResult.HasAuthorshipNote
        stats = (Convert-ObjectKeysToCamelCase -Value $StatsResult.Stats)
    }
}

function Test-BatchUploadItemSucceeded {
    param([object]$ResponseItem)

    $success = Get-ResponsePropertyValue -Object $ResponseItem -Names @('success', 'succeeded', 'isSuccess')
    if ($null -ne $success) {
        return [bool]$success
    }

    $status = Get-ResponsePropertyValue -Object $ResponseItem -Names @('status', 'result')
    if ($status) {
        return @('uploaded', 'upserted', 'created', 'updated', 'ok', 'success', 'accepted') -contains ([string]$status).ToLowerInvariant()
    }

    return $true
}

function Convert-BatchUploadResponse {
    <#
    .SYNOPSIS 将远端返回的 results[] 规范化成按 commit 汇总的结果列表
    #>
    param(
        [object]$Response,
        [object[]]$CommitItems
    )

    $responseItems = @(Get-ResponsePropertyValue -Object $Response -Names @('results', 'commits', 'items'))
    if (-not $responseItems -or $responseItems.Count -eq 0) {
        return @($CommitItems | ForEach-Object {
            @{
                commitSha = [string]$_.commitSha
                succeeded = $true
                status = 'uploaded'
                error = $null
                hasAuthorshipNote = [bool]$_.hasAuthorshipNote
                stats = $_.stats
            }
        })
    }

    $responseBySha = @{}
    foreach ($responseItem in $responseItems) {
        $sha = Get-ResponsePropertyValue -Object $responseItem -Names @('commitSha', 'commit_sha', 'sha')
        if ($sha) {
            $responseBySha[[string]$sha] = $responseItem
        }
    }

    $normalized = @()
    foreach ($commitItem in $CommitItems) {
        $responseItem = if ($responseBySha.ContainsKey([string]$commitItem.commitSha)) {
            $responseBySha[[string]$commitItem.commitSha]
        } else {
            $null
        }

        $succeeded = if ($responseItem) {
            Test-BatchUploadItemSucceeded -ResponseItem $responseItem
        } else {
            $true
        }

        $status = if ($responseItem) {
            Get-ResponsePropertyValue -Object $responseItem -Names @('status', 'result')
        } else {
            $null
        }

        if (-not $status) {
            $status = if ($succeeded) { 'uploaded' } else { 'failed' }
        }

        $error = if ($responseItem) {
            Get-ResponsePropertyValue -Object $responseItem -Names @('error', 'errorMessage', 'message', 'reason')
        } else {
            $null
        }

        $normalized += @{
            commitSha = [string]$commitItem.commitSha
            succeeded = $succeeded
            status = [string]$status
            error = if ($error) { [string]$error } else { $null }
            hasAuthorshipNote = [bool]$commitItem.hasAuthorshipNote
            stats = $commitItem.stats
        }
    }

    return $normalized
}

function Send-AiStatsBatchToRemote {
    <#
    .SYNOPSIS 将多个 commit 的 AI 统计一次性 POST 到远程 API
    .DESCRIPTION
        构造批量请求体 → 读取 endpoint / api_key → 一次发送。
        服务端按 commitSha 做幂等去重，并通过 results[] 返回每个 commit 的状态。
    #>
    param(
        [object[]]$CommitItems,
        [string]$ProjectName,
        [hashtable]$RemoteConfig,
        [string]$Source,
        [string]$ReviewDocumentId
    )

    $repoRoot = Get-RepoRoot
    $repoUrl = git -C $repoRoot remote get-url origin 2>$null
    $branch = git -C $repoRoot rev-parse --abbrev-ref HEAD 2>$null

    $payload = @{
        repoUrl = $repoUrl
        projectName = $ProjectName
        branch = $branch
        source = (Get-NormalizedUploadSource -Value $Source)
        reviewDocumentId = if ($ReviewDocumentId) { $ReviewDocumentId } else { $null }
        authorshipSchemaVersion = "authorship/3.0.0"
        commits = $CommitItems
    } | ConvertTo-Json -Depth 12

    $headers = @{ "Content-Type" = "application/json" }
    if ($RemoteConfig.ApiKey) { $headers["Authorization"] = "Bearer $($RemoteConfig.ApiKey)" }
    if ($RemoteConfig.UserId) { $headers["X-USER-ID"] = [string]$RemoteConfig.UserId }

    try {
        $response = Invoke-RestMethod -Uri $RemoteConfig.Url `
            -Method POST -Body $payload -Headers $headers -TimeoutSec 10

        return @{
            Succeeded = $true
            Results = @(Convert-BatchUploadResponse -Response $response -CommitItems $CommitItems)
        }
    } catch {
        Write-Warning "[upload-ai-stats] 批量上传失败: $_"
        return @{
            Succeeded = $false
            Results = @($CommitItems | ForEach-Object {
                @{
                    commitSha = [string]$_.commitSha
                    succeeded = $false
                    status = 'failed'
                    error = 'batch request failed'
                    hasAuthorshipNote = [bool]$_.hasAuthorshipNote
                    stats = $_.stats
                }
            })
        }
    }
}

# ─── 主流程 ──────────────────────────────────────────────────

# 第 1 步：检测 git-ai 是否可用
$gitAiCmd = Get-Command git-ai -ErrorAction SilentlyContinue
if (-not $gitAiCmd) {
    Write-Error "[upload-ai-stats] git-ai 未安装！请先执行: .\.specify\scripts\powershell\post-init.ps1"
    exit 1
}

# 第 2 步：收集目标 commit
$commits = Get-TargetCommits
if ($commits.Count -eq 0) {
    Write-Host "[upload-ai-stats] 未找到匹配的 commit（可能当前分支 = 基分支？）"
    exit 0
}

Write-Host "[upload-ai-stats] 找到 $($commits.Count) 个 commit，正在收集 AI 统计..."
Write-Host ""

# 第 3 步：获取项目名（从 remote URL 推导）
$repoRoot = Get-RepoRoot
$repoUrl = git -C $repoRoot remote get-url origin 2>$null
$projectName = ($repoUrl -split '/')[-1] -replace '\.git$', ''
$remoteConfig = $null

if (-not $DryRun) {
    $remoteConfig = Get-UploadRemoteConfig
    if (-not $remoteConfig) {
        exit 1
    }
}

# 第 4 步：逐个 commit 获取统计，汇总后一次批量上传
$results = @()
$preparedCommitItems = @()
$successCount = 0
$skipCount = 0
$failCount = 0
$withoutNoteCount = 0

foreach ($sha in $commits) {
    $shortSha = $sha.Substring(0, [Math]::Min(7, $sha.Length))
    
    $statsResult = Get-CommitAiStats -CommitSha $sha
    if (-not $statsResult) {
        Write-Host "  $shortSha : 统计读取失败，跳过" -ForegroundColor DarkGray
        $skipCount++
        continue
    }

    $commitItem = New-CommitUploadItem -CommitSha $sha -StatsResult $statsResult
    if (-not $commitItem) {
        Write-Host "  $shortSha : commit 元数据读取失败，跳过" -ForegroundColor DarkGray
        $skipCount++
        continue
    }

    $stats = $commitItem.stats
    $hasAuthorshipNote = [bool]$commitItem.hasAuthorshipNote
    if (-not $hasAuthorshipNote) { $withoutNoteCount++ }

    if ($DryRun) {
        if ($hasAuthorshipNote) {
            Write-Host "  $shortSha : [预览] note=有, 新增=$($stats.gitDiffAddedLines) 行, aiAdditions=$($stats.aiAdditions), humanAdditions=$($stats.humanAdditions), unknownAdditions=$($stats.unknownAdditions)" -ForegroundColor Cyan
        } else {
            Write-Host "  $shortSha : [预览] note=无, 新增=$($stats.gitDiffAddedLines) 行, humanAdditions=$($stats.humanAdditions), unknownAdditions=$($stats.unknownAdditions)" -ForegroundColor Yellow
        }
        $results += @{ commitSha = $sha; succeeded = $true; status = "dry-run"; hasAuthorshipNote = $hasAuthorshipNote; stats = $stats }
        continue
    }

    $preparedCommitItems += $commitItem
}

if (-not $DryRun -and $preparedCommitItems.Count -gt 0) {
    $batchUploadResult = Send-AiStatsBatchToRemote -CommitItems $preparedCommitItems -ProjectName $projectName -RemoteConfig $remoteConfig -Source $Source -ReviewDocumentId $ReviewDocumentId

    foreach ($uploadResult in $batchUploadResult.Results) {
        $shortSha = $uploadResult.commitSha.Substring(0, [Math]::Min(7, $uploadResult.commitSha.Length))

        if ($uploadResult.succeeded) {
            if ($uploadResult.hasAuthorshipNote) {
                Write-Host "  $shortSha : ✓ 已上传 (note=有, 新增=$($uploadResult.stats.gitDiffAddedLines), aiAdditions=$($uploadResult.stats.aiAdditions), humanAdditions=$($uploadResult.stats.humanAdditions), unknownAdditions=$($uploadResult.stats.unknownAdditions))" -ForegroundColor Green
            } else {
                Write-Host "  $shortSha : ✓ 已上传 (note=无, 新增=$($uploadResult.stats.gitDiffAddedLines), humanAdditions=$($uploadResult.stats.humanAdditions), unknownAdditions=$($uploadResult.stats.unknownAdditions))" -ForegroundColor Green
            }
            $successCount++
        } else {
            $errorSuffix = if ($uploadResult.error) { " ($($uploadResult.error))" } else { "" }
            Write-Host "  $shortSha : ✗ 上传失败$errorSuffix" -ForegroundColor Red
            $failCount++
        }

        $resultEntry = @{
            commitSha = $uploadResult.commitSha
            succeeded = [bool]$uploadResult.succeeded
            status = [string]$uploadResult.status
            hasAuthorshipNote = [bool]$uploadResult.hasAuthorshipNote
            stats = $uploadResult.stats
        }
        if ($uploadResult.error) {
            $resultEntry['error'] = [string]$uploadResult.error
        }
        $results += $resultEntry
    }
}

# 第 5 步：汇总输出
Write-Host ""
if ($DryRun) {
    Write-Host "[upload-ai-stats] [预览模式] 共 $($results.Count) 个 commit 生成统计，其中 $withoutNoteCount 个无 authorship note，$skipCount 个读取失败被跳过"
    Write-Host "[upload-ai-stats] 去掉 -DryRun 参数即可真正上传"
} else {
    Write-Host "[upload-ai-stats] ✓ 完成：$successCount 成功, $failCount 失败, $skipCount 跳过, $withoutNoteCount 个无 authorship note"
}

if ($Json) {
    $results | ConvertTo-Json -Depth 10
}
```

**验证方法：** 创建完脚本后，先用 DryRun 模式验证：

```powershell
# 测试命令（不会真正上传）
.\.specify\scripts\powershell\upload-ai-stats.ps1 -DryRun

# 预期输出：
# [upload-ai-stats] 找到 5 个 commit，正在收集 AI 统计...
#
#   abc1234 : [预览] note=有, 新增=200 行, aiAdditions=80, humanAdditions=105, unknownAdditions=15
#   def5678 : [预览] note=有, 新增=150 行, aiAdditions=30, humanAdditions=115, unknownAdditions=5
#   gh90abc : [预览] note=无, 新增=120 行, humanAdditions=0, unknownAdditions=120
#   ...
#
# [upload-ai-stats] [预览模式] 共 5 个 commit 生成统计，其中 1 个无 authorship note，0 个读取失败被跳过
```

---

#### 第 2 步：注册为 Speckit Agent 命令（可选）

**要做什么：** 在 Agent prompt 系统中注册一个触发词，让用户可以通过自然语言调用上传功能。

**为什么：** 有些团队成员不喜欢记命令行路径，直接对 AI Agent 说"上传 AI 统计"更方便。

**具体怎么做：** 在 `.github/agents/` 下新建或编辑一个 agent prompt 文件，加入以下规则：

```markdown
<!-- 在合适的 agent prompt 中添加以下触发规则 -->

### 触发词: "上传 AI 统计" / "upload ai stats"

当用户说 "上传 AI 统计"、"upload ai stats" 或类似意图时：

1. 在终端执行: `.specify/scripts/powershell/upload-ai-stats.ps1`
2. 如果用户指定了日期范围，追加 `-Since` / `-Until` 参数
3. 如果用户指定了 commit，追加 `-Commits` 参数
4. 展示上传结果摘要
```

---

### 3.4 路径 B 详细实施：Code Review 时自动上传

#### 背景知识：Code Review Agent 当前的步骤 8 长什么样？

现有的 `.github/agents/speckit.code-review.agent.md` 文件中，步骤 8 是"同步问题清单到远程服务器"，包含两个子步骤：

```
步骤 8.1: 调用 mcp_upload-doc_create_code_review_document
          → 在远程创建一个"审查文档"
          → 返回一个 documentId（后续步骤用这个 ID 关联数据）

步骤 8.2: 调用 mcp_upload-doc_create_code_review_issue × N
          → 逐个创建审查中发现的问题条目
          → 每个 issue 关联到 documentId
```

**我们要做的：在 8.2 之后追加 8.3 和 8.4，把 AI 使用统计数据也上传上去。**

#### 具体操作步骤

**第 1 步：修改 `speckit.code-review.agent.md`**

**要做什么：** 在 Code Review Agent prompt 的步骤 8.2 之后，追加步骤 8.3 和 8.4：

```markdown
### 步骤 8.3：收集被审查 commit 的 AI 归因数据

在步骤 8.2 完成后，对本次 Code Review 涉及的每个 commit 收集 AI 使用统计：

1. **检测 git-ai 是否可用**
   - 在终端执行: `git-ai --version`
   - 如果命令不存在（未安装），直接跳到步骤 9，不影响审查流程
   - 如果命令存在，继续下一步

2. **对每个被审查的 commit 获取统计**
    - 先执行: `git notes --ref=ai list <commit_full_hash>`，把结果记成 `hasAuthorshipNote`
    - 无论有没有 note，都执行: `git-ai stats <commit_full_hash> --json`
    - 如果 `git-ai stats` 成功返回 JSON，就记录下来；无 note 的 commit 仍然保留，但要在结果里标记 `hasAuthorshipNote=false`

3. **收集结果汇总**
   - 将所有成功拿到 stats JSON 的 commit SHA 用逗号拼接
   - 如果一个都没有，跳到步骤 9

### 步骤 8.4：上传 AI 统计到远程

在步骤 8.3 收集到有效数据的前提下：

1. **调用上传脚本**
   - 执行: `.specify/scripts/powershell/upload-ai-stats.ps1 -Commits "<逗号分隔的SHA>"`
    - 如果未显式配置 URL，脚本默认调用 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`
    - 如需覆盖，脚本会优先读取 `GIT_AI_REPORT_REMOTE_URL`
    - 也可通过 `GIT_AI_REPORT_REMOTE_ENDPOINT` + `GIT_AI_REPORT_REMOTE_PATH` 覆盖，默认值分别为 `https://service-gw.ruijie.com.cn` 和 `/api/ai-cr-manage-service/api/public/upload/ai-stats`
    - `GIT_AI_REPORT_REMOTE_API_KEY` 用于认证（可选）
    - `GIT_AI_REPORT_REMOTE_USER_ID` 如果存在，会优先作为 `X-USER-ID` 请求头
    - 如果未设置 `GIT_AI_REPORT_REMOTE_USER_ID`，脚本会继续尝试从本机 VS Code / IDEA 的 MCP 配置中读取 `X-USER-ID`
    - 可通过 `GIT_AI_VSCODE_MCP_CONFIG_PATH` / `GIT_AI_IDEA_MCP_CONFIG_PATH` 覆盖默认配置文件探测路径
     - 脚本会把所有目标 commit 组装成一次批量请求，并按 `results[]` 逐条解析返回结果

2. **记录上传结果**
   - 如果上传成功，在审查报告末尾追加「AI 代码使用统计」表格（格式见下方模板）
    - 如果批量上传失败、部分 commit 上传失败或未配置 endpoint，记录警告但不影响审查报告的其他内容

> ⚠️ 重要：步骤 8.3/8.4 的任何失败都不应该阻止审查报告的生成。
> git-ai 数据是"锦上添花"，不是"刚需"。
```

**第 2 步：在审查报告模板中追加 AI 统计章节**

**要做什么：** 在 Code Review 生成的报告末尾，自动追加一个 AI 使用统计表格。

**为什么：** 让 leader 在看审查报告时，一眼就能看到每个 commit 的 AI 使用比例，不需要另外查询。

**追加的报告内容模板：**

```markdown
## AI 代码使用统计

| Commit | 作者 | 总新增行 | AI归因新增 | 已知人工 | 未知/未归因 | AI 占比 | Note | 主要工具 |
|--------|------|---------|-----------|---------|------------|---------|------|---------|
| abc123d | 张三 | 200 | 80 | 105 | 15 | 40% | 有 | copilot / gpt-4o |
| def456a | 张三 | 150 | 0 | 90 | 60 | 0% | 无 | — |
| **合计** | — | **350** | **80** | **195** | **75** | **23%** | — | — |

> **数据来源：** git-ai authorship note (`refs/notes/ai`)
> **AI 占比** = `stats.aiAdditions / stats.gitDiffAddedLines`
> **当前默认展示口径** = `stats.aiAdditions`、`stats.humanAdditions`、`stats.unknownAdditions`
> **mixedAdditions** = 仍保留在原始 `stats` 中，但当前预览和报告摘要不单独展示
> **如果某些 commit 无 note：** 不应直接跳过；应显示为 `hasAuthorshipNote=false`，并把未归因新增行保留在 `unknownAdditions`
```

**表格中每列的含义：**

| 列名 | 数据来源 | 含义 |
|------|---------|------|
| Commit | `git log --format=%h` | 短 SHA，点击可定位到具体 commit |
| 作者 | `git log --format=%ae` | 该 commit 的作者邮箱 |
| 总新增行 | `stats.gitDiffAddedLines` | git diff 统计的新增行数 |
| 已知人工 | `stats.humanAdditions` | 有明确 KnownHuman 归因的新增行数 |
| 未知/未归因 | `stats.unknownAdditions` | 当前没有 attestation 的新增行数；无 note 时通常会占大头 |
| AI归因新增 | `stats.aiAdditions` | 当前默认对外展示的 AI 新增行数，已包含 mixedAdditions |
| AI 占比 | 计算值 | `stats.aiAdditions / stats.gitDiffAddedLines × 100%` |
| Note | `hasAuthorshipNote` | 当前 commit 是否真的带有 `refs/notes/ai` |
| 主要工具 | `stats.toolModelBreakdown` 中 `aiAdditions` 最大的项，展示为 `tool / model` | 如 `copilot / gpt-4o` |

---

### 3.5 远程 API 请求体设计

**两条路径共享同一个 batch API 语义和数据格式。** 这样做的好处是：

1. 客户端只发一次请求，不再为每个 commit 单独发 POST。
2. 服务端仍然保持 commit 粒度入库，并按 `commitSha` 做幂等去重；逐文件明细作为 `stats.files[]` 挂在 commit 记录下面。
3. 响应可以通过 `results[]` 明确告诉客户端哪些 commit 成功、哪些失败。

**API 地址：** 按当前外网实测，最终调用地址为 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`，并且当前 batch 请求体可返回 200。也可以通过 `base endpoint + path` 组合显式覆盖。

**补充约定：** `git-ai stats <sha> --json` 的原始输出仍然是 snake_case；客户端脚本在上传前会统一转换为 Java 接口使用的 camelCase DTO。

**请求体（JSON 格式）：**

```json
{
    "repoUrl": "https://gitlab.example.com/team/project.git",
    "projectName": "my-service",
    "branch": "001-user-auth",
    "source": "manual",
    "reviewDocumentId": null,
    "authorshipSchemaVersion": "authorship/3.0.0",
    "commits": [
        {
            "commitSha": "abc123def456789...",
            "commitMessage": "feat: add user auth",
            "author": "developer@example.com",
            "timestamp": "2026-04-14 12:00:00",
            "hasAuthorshipNote": true,
            "stats": {
                "humanAdditions": 105,
                "unknownAdditions": 15,
                "aiAdditions": 80,
                "aiAccepted": 65,
                "mixedAdditions": 15,
                "totalAiAdditions": 95,
                "totalAiDeletions": 10,
                "gitDiffAddedLines": 200,
                "gitDiffDeletedLines": 30,
                "timeWaitingForAi": 45,
                "files": [
                    {
                        "filePath": "src/auth/service/AuthService.java",
                        "gitDiffAddedLines": 120,
                        "gitDiffDeletedLines": 18,
                        "aiAdditions": 55,
                        "humanAdditions": 45,
                        "unknownAdditions": 20,
                        "toolModelBreakdown": [
                            {
                                "tool": "copilot",
                                "model": "gpt-4o",
                                "aiAdditions": 35
                            },
                            {
                                "tool": "cursor",
                                "model": "claude-sonnet",
                                "aiAdditions": 20
                            }
                        ]
                    },
                    {
                        "filePath": "src/auth/web/AuthController.java",
                        "gitDiffAddedLines": 80,
                        "gitDiffDeletedLines": 12,
                        "aiAdditions": 25,
                        "humanAdditions": 60,
                        "unknownAdditions": 5,
                        "toolModelBreakdown": []
                    }
                ],
                "toolModelBreakdown": [
                    {
                        "tool": "copilot",
                        "model": "gpt-4o",
                        "aiAdditions": 50,
                        "aiAccepted": 40,
                        "mixedAdditions": 10
                    },
                    {
                        "tool": "cursor",
                        "model": "claude-sonnet",
                        "aiAdditions": 30,
                        "aiAccepted": 25,
                        "mixedAdditions": 5
                    }
                ]
            }
        },
        {
            "commitSha": "def456abc987654...",
            "commitMessage": "fix: tighten auth checks",
            "author": "developer@example.com",
            "timestamp": "2026-04-14 12:10:00",
            "hasAuthorshipNote": false,
            "stats": {
                "humanAdditions": 40,
                "unknownAdditions": 12,
                "aiAdditions": 0,
                "aiAccepted": 0,
                "mixedAdditions": 0,
                "totalAiAdditions": 0,
                "totalAiDeletions": 0,
                "gitDiffAddedLines": 52,
                "gitDiffDeletedLines": 8,
                "timeWaitingForAi": 0,
                "files": [
                    {
                        "filePath": "src/auth/web/AuthFilter.java",
                        "gitDiffAddedLines": 52,
                        "gitDiffDeletedLines": 8,
                        "aiAdditions": 0,
                        "humanAdditions": 40,
                        "unknownAdditions": 12,
                        "toolModelBreakdown": []
                    }
                ],
                "toolModelBreakdown": []
            }
        }
    ]
}
```

**响应体（建议格式）：**

```json
{
    "total": 2,
    "succeeded": 1,
    "failed": 1,
    "results": [
        {
            "commitSha": "abc123def456789...",
            "succeeded": true,
            "status": "upserted"
        },
        {
            "commitSha": "def456abc987654...",
            "succeeded": false,
            "status": "failed",
            "errorMessage": "invalid stats payload"
        }
    ]
}
```

**为什么这样设计：**

- **批量只是传输层优化**：请求一次带多个 commit，但存储和幂等仍然是 commit 粒度。
- **部分失败可追踪**：`results[]` 允许一个请求里同时出现成功和失败，客户端可以精确提示或重试。
- **兼容 Code Review 关联**：`reviewDocumentId` 仍保留在批量请求顶层，统一关联本次审查。

**请求体顶层字段说明：**

| 字段 | 含义 | 来源 | 备注 |
|------|------|------|------|
| `repoUrl` | 仓库远程地址 | `git remote get-url origin` | 服务端按项目归类 |
| `projectName` | 项目名称 | 从 repoUrl 提取最后一段 | 仪表盘展示用 |
| `branch` | 分支名 | `git rev-parse --abbrev-ref HEAD` | 辅助信息，整个批次共用 |
| `source` | 上传来源 | 脚本或审查流程指定 | `"manual"` 或 `"codeReview"` |
| `reviewDocumentId` | 关联的审查文档 ID | Code Review 步骤 8.1 返回值 | 主动上传时为 null |
| `authorshipSchemaVersion` | 数据格式版本 | 固定值 `"authorship/3.0.0"` | 服务端兼容不同版本用 |
| `commits` | 本次批量提交的 commit 列表 | 客户端组装 | 每个元素仍然对应一个 commit |

**`commits[]` 内部字段说明：**

| 字段 | 含义 | 来源 | 备注 |
|------|------|------|------|
| `commitSha` | 完整 commit SHA | `git log --format=%H` | **唯一键之一** |
| `commitMessage` | 提交消息 | `git log --format=%s` | 仪表盘展示用 |
| `author` | 作者邮箱 | `git log --format=%ae` | 按成员统计用 |
| `timestamp` | 提交时间 | `git log --format=%aI` 后由脚本格式化为 `yyyy-MM-dd HH:mm:ss` | 时间线展示用 |
| `hasAuthorshipNote` | 是否有完整的归因数据 | `git notes --ref=ai list <sha>` 有输出则为 true | 区分"有归因记录"和"没有记录" |
| `stats` | AI 使用统计详情 | `git-ai stats <sha> --json` 输出 | 核心数据 |

**`stats` 内部字段详解：**

| stats 字段 | 含义 | 举例 |
|------------|------|------|
| `humanAdditions` | 已知有人类 attestation 的新增行数（KnownHuman） | 105 行 |
| `unknownAdditions` | 当前没有 attestation 的新增行数；没有 note 时通常会很高 | 15 行 |
| `aiAdditions` | 带有 AI 归因的新增行数，等于 `aiAccepted + mixedAdditions` | 80 行 |
| `aiAccepted` | AI 生成且最终未被人工改动的行数 | 65 行 |
| `mixedAdditions` | AI 和人工混合编辑的行数 | 15 行 |
| `totalAiAdditions` | 本次开发过程中 AI 一共生成过多少行，可能大于最终提交中的 `aiAdditions` | 95 行 |
| `totalAiDeletions` | AI 参与的删除行数 | 10 行 |
| `gitDiffAddedLines` | git diff 统计的总新增行数 | 200 行 |
| `gitDiffDeletedLines` | git diff 统计的总删除行数 | 30 行 |
| `timeWaitingForAi` | 等待 AI 响应的总时间（秒） | 45 秒 |
| `files` | 逐文件 commit-local 归因明细数组；由 `Get-CommitAiFileStats` 解析 authorship note attestation + `git diff-tree --numstat` 产出 | 见上面 JSON 示例 |
| `toolModelBreakdown` | 按工具+模型分组的细分数据数组；每项包含 `tool`、`model`、`aiAdditions`、`aiAccepted`、`mixedAdditions` | 见上面 JSON 示例 |

**`stats.files[]` 内部字段详解：**

> **数据来源：** `git diff-tree --no-commit-id --numstat -r <sha>`（行数）+ `git notes --ref=ai show <sha>`（attestation 归因）。这是 commit-local 语义，只反映当前 commit 自身的 AI/人工归因，不做跨 commit 的 provenance 追溯。

| files 字段 | 含义 | 举例 |
|------------|------|------|
| `filePath` | 文件路径（来自 `git diff-tree --numstat`） | `src/auth/service/AuthService.java` |
| `gitDiffAddedLines` | 该文件在本次 commit 中的新增行数（来自 `git diff-tree --numstat`） | 120 行 |
| `gitDiffDeletedLines` | 该文件在本次 commit 中的删除行数（来自 `git diff-tree --numstat`） | 18 行 |
| `aiAdditions` | 该文件中 AI 归因新增行数（来自 authorship note attestation，非 `h_*` 前缀的条目） | 55 行 |
| `humanAdditions` | 该文件中已知人工新增行数（来自 authorship note attestation，`h_*` 前缀的条目） | 45 行 |
| `unknownAdditions` | 该文件中未归因新增行数（`gitDiffAddedLines - aiAdditions - humanAdditions`） | 20 行 |
| `toolModelBreakdown` | 该文件维度的工具/模型分解（来自 authorship note JSON 元数据的 `prompts.<hash>.agent_id`）；当前稳定提供 `aiAdditions` | 见上面 JSON 示例 |

**响应体 `results[]` 字段说明：**

| 字段 | 含义 | 备注 |
|------|------|------|
| `commitSha` | 这条结果对应的 commit | 用于和客户端原始请求一一对应 |
| `succeeded` | 单个 commit 是否处理成功 | Java DTO 可直接映射为布尔字段 |
| `status` | 处理状态 | 建议值：`upserted`、`failed`、`skipped` |
| `errorMessage` | 失败原因 | 仅失败时返回 |

---

### 3.6 配置项（环境变量 + IDE MCP X-USER-ID 自动探测）

**要做什么：** 通过环境变量告诉上传脚本"往哪里发"和"用什么认证"。

> **与最新 git-ai 原生能力的边界：** `GIT_AI_REPORT_REMOTE_URL` / `GIT_AI_REPORT_REMOTE_ENDPOINT` / `GIT_AI_REPORT_REMOTE_PATH` / `GIT_AI_REPORT_REMOTE_API_KEY` / `GIT_AI_REPORT_REMOTE_USER_ID` / `GIT_AI_VSCODE_MCP_CONFIG_PATH` / `GIT_AI_IDEA_MCP_CONFIG_PATH` 只服务于本文这条“自建上传脚本”路径；如果改用 Git AI 官方或自托管后端，应改配 `GIT_AI_API_KEY`，必要时再配 `GIT_AI_API_BASE_URL`。

**为什么不用 `git-ai config set`？**
- 当前 `git-ai config` 不支持 `report_to_remote.endpoint` / `report_to_remote.api_key` 这类自定义键
- 这个集成方案承诺“不改 git-ai Rust 代码”，那最稳妥的做法就是让上传脚本直接读取环境变量
- API key 不应该落到仓库文件里；环境变量和 CI Secret 更符合当前约束

**X-USER-ID 的读取优先级：**
- `GIT_AI_REPORT_REMOTE_USER_ID`
- 本机 VS Code MCP 配置中的 `headers.X-USER-ID`
- 本机 IDEA MCP 配置中的 `headers.X-USER-ID` 或 `requestInit.headers.X-USER-ID`

**默认探测路径（Windows）：**
- VS Code: `%APPDATA%\Code\User\mcp.json`，并兼容同目录下的 `settings.json`
- IDEA: 优先探测 `%LOCALAPPDATA%\github-copilot\intellij\mcp.json` 与 `%APPDATA%\github-copilot\intellij\mcp.json`
- IDEA: 同时兼容 `%APPDATA%\JetBrains\**\*.json` 与 `%LOCALAPPDATA%\JetBrains\**\*.json` 中包含 MCP 关键字的 JSON/JSONC 文件
- 如果默认探测路径不适用，可显式设置 `GIT_AI_VSCODE_MCP_CONFIG_PATH` 或 `GIT_AI_IDEA_MCP_CONFIG_PATH`

**关于 URL 示例的说明：**
- 当前最终调用接口为 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`
- 如果未显式配置 `GIT_AI_REPORT_REMOTE_URL` / `GIT_AI_REPORT_REMOTE_ENDPOINT` / `GIT_AI_REPORT_REMOTE_PATH`，上传脚本会默认调用该地址
- 如果按完整 URL 显式配置，就写成 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`
- 如果按 `endpoint + path` 拆分配置，则 `GIT_AI_REPORT_REMOTE_ENDPOINT` 应写成 `https://service-gw.ruijie.com.cn`，`GIT_AI_REPORT_REMOTE_PATH` 应写成 `/api/ai-cr-manage-service/api/public/upload/ai-stats`

**配置方法：**

```powershell
# Windows PowerShell：写入用户级环境变量（新开一个 shell 生效）
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_URL", "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats", "User")
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_API_KEY", "your-personal-api-key", "User")
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_USER_ID", "your-user-id", "User")
```

如果用户已经在 VS Code / IDEA 的 MCP 配置里写了 `X-USER-ID`，也可以不再额外设置 `GIT_AI_REPORT_REMOTE_USER_ID`；脚本会优先读取显式环境变量，未设置时再回退到 IDE MCP 配置。

```bash
# macOS / Linux：写入 shell profile
export GIT_AI_REPORT_REMOTE_URL="https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
export GIT_AI_REPORT_REMOTE_API_KEY="your-personal-api-key"
export GIT_AI_REPORT_REMOTE_USER_ID="your-user-id"
```

**CI/CD 或临时覆盖：**

```bash
export GIT_AI_REPORT_REMOTE_URL="https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
export GIT_AI_REPORT_REMOTE_API_KEY="ci-service-account-key"
export GIT_AI_REPORT_REMOTE_USER_ID="ci-service-user"
```

**验证配置是否正确：**

```powershell
Write-Host $env:GIT_AI_REPORT_REMOTE_URL
Write-Host ($env:GIT_AI_REPORT_REMOTE_API_KEY.Substring(0,4) + "***")
Write-Host $env:GIT_AI_REPORT_REMOTE_USER_ID
```

```bash
echo "$GIT_AI_REPORT_REMOTE_URL"
printf '%.4s***\n' "$GIT_AI_REPORT_REMOTE_API_KEY"
echo "$GIT_AI_REPORT_REMOTE_USER_ID"
```

---

## 四、端到端流程——三个场景，一步步走

> **为什么要写端到端流程？** 上面的内容是"每个零件怎么造"，这里是"零件装好后，整辆车怎么开"。
> 每个场景都从"用户的第一个动作"开始，到"最终可以看到的结果"结束。

### 场景 1：新成员入职——从 clone 到开始开发

**前提：** 团队已经在项目中添加了 `.specify/` 目录和 `post-init.ps1`（需求 1 的产物）

**完整步骤：**

```
第 1 步：clone 项目仓库
  ┌─────────────────────────────────────────┐
  │  git clone https://gitlab.com/team/proj │
  │  cd proj                                │
  └─────────────────────────────────────────┘
  结果：拿到代码，仓库中已包含 .specify/ 目录

第 2 步：执行 Speckit 初始化（或由包装链路 / Agent 触发 post-init）
  ┌─────────────────────────────────────────┐
    │  specify init . --ai copilot             │
    │                                          │
    │  或更新已有项目：                         │
    │  specify init --here --force --ai copilot│
        │      --script ps                         │
  └─────────────────────────────────────────┘
        补充说明：
                ① 如果执行的是裸命令 `specify init . --ai copilot`，当前仓库无法在命令内部自动注入 post-init
                ② 正确做法是在命令成功后立刻补执行 `pwsh .specify/scripts/powershell/post-init.ps1`
                ③ 如果执行的是仓库内包装过的更新脚本（如 `batch-update.ps1`），则 post-init 会自动跟随执行
  
    post-init 脚本内部执行顺序：
        ① 检测 git-ai 命令，或回退检查默认安装路径 ~/.git-ai/bin
        ② 如果未安装，从官方安装地址下载并执行安装器
              （默认是 `https://usegitai.com/install.ps1`，也支持 GIT_AI_INSTALLER_URL 覆盖）
          ③ 对已安装场景执行 `git-ai install-hooks` → 刷新 IDE / Agent hooks 配置
          ④ 如果当前 shell PATH 还没刷新，脚本会给出手动补跑 `git-ai install-hooks` 的提示，但不会让 init 失败
  
    预期输出：
        [speckit/post-init] Downloading git-ai installer...
        [speckit/post-init] git-ai installed successfully: git-ai x.y.z
        [speckit/post-init] git-ai install-hooks completed successfully.

第 3 步：如需显式固化默认地址，再配置远程 API（当前实现默认不会自动写环境变量）
    ┌──────────────────────────────────────────────────────────────────────────────┐
        │  [Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_URL",         │
                │      "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats", "User") │
    │  [Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_API_KEY",     │
    │      "your-key", "User")                                                  │
    └──────────────────────────────────────────────────────────────────────────────┘
  如何获取这两个值？向团队 leader 要：
        - remote URL: 默认内置为最终接口；如需显式写入，直接使用本文固定 URL
  - api_key: 每人一个，leader 分配

第 4 步：正常开发
  ┌───────────────────────────────────────────────────────────────┐
  │  vim src/main.rs                                             │
  │  git add . && git commit -m "feat: something"                │
  └───────────────────────────────────────────────────────────────┘
  每次 commit 时，git-ai 自动在本地记录 authorship note
  （完全透明，不影响 commit 速度，不联网）
```

**验证点：** 执行 `git-ai --version` 能看到版本号 = 安装成功

---

### 场景 2：功能开发完成 → 主动上传 AI 统计

**前提：** 开发者已在功能分支上完成多次 commit

**完整步骤：**

```
第 1 步：确认自己在功能分支上
  ┌───────────────────────────────────────┐
  │  git branch                           │
  │  # * 001-user-auth  ← 当前分支        │
  └───────────────────────────────────────┘

第 2 步：先预览看看有什么数据（推荐）
  ┌───────────────────────────────────────────────────────────────┐
  │  .\.specify\scripts\powershell\upload-ai-stats.ps1 -DryRun   │
  └───────────────────────────────────────────────────────────────┘
  预期输出：
    [upload-ai-stats] 找到 5 个 commit，正在收集 AI 统计...
    
                        abc1234 : [预览] note=有, 新增=200 行, AI归因=80 行, 混编=15 行
                        def5678 : [预览] note=有, 新增=150 行, AI归因=30 行, 混编=5 行
                        gh90abc : [预览] note=无, 新增=120 行, unknown=120 行
      ...
    
        [upload-ai-stats] [预览模式] 共 5 个 commit 生成统计，其中 1 个无 authorship note

第 3 步：确认无误后，真正上传
  ┌───────────────────────────────────────────────────────────────┐
  │  .\.specify\scripts\powershell\upload-ai-stats.ps1            │
  └───────────────────────────────────────────────────────────────┘
  预期输出：
    [upload-ai-stats] 找到 5 个 commit，正在收集 AI 统计...
    
                        abc1234 : ✓ 已上传 (note=有, 新增=200, AI归因=80, 混编=15)
                        def5678 : ✓ 已上传 (note=有, 新增=150, AI归因=30, 混编=5)
                        gh90abc : ✓ 已上传 (note=无, 新增=120, unknown=120)
      ...
    
        [upload-ai-stats] ✓ 完成：5 成功, 0 失败, 0 跳过, 1 个无 authorship note

第 4 步：提交 PR / MR（正常流程）
  └─ 数据已在远端，leader 可以在仪表盘上看到
```

**验证点：** 上传后在仪表盘页面能看到自己的 commit 统计数据

---

### 场景 3：Code Review 时 → 自动上传

**前提：** Reviewer 使用 Speckit Code Review Agent 进行审查

**完整步骤：**

```
第 1 步：Reviewer 触发 Code Review
  ┌───────────────────────────────────────┐
  │  /speckit.code-review                 │
  │  （在 VS Code / IDE 中输入命令）       │
  └───────────────────────────────────────┘

第 2-7 步：（Speckit 自动执行，无需人工干预）
  ├─ 收集代码变更
  ├─ 逐 commit 分析
  └─ 生成审查报告

第 8.1-8.2 步：同步到远程（已有功能，无需修改）
  ├─ 创建审查文档 → 返回 documentId
  └─ 逐个创建问题条目

  ★ 第 8.3 步（新增）：收集 AI 统计
  ├─ 检测 git-ai --version
  │   ├─ 未安装 → 跳过 8.3/8.4，不影响审查
  │   └─ 已安装 → 继续
  └─ 对每个被审查 commit 执行 git-ai stats <sha> --json
    （如果 `git notes --ref=ai list <sha>` 无输出，也保留该 commit，只是标记 `hasAuthorshipNote=false`）
  
  ★ 第 8.4 步（新增）：上传 AI 统计
  ├─ 调用 upload-ai-stats.ps1 -Commits "sha1,sha2,..."
  └─ 在审查报告末尾追加「AI 代码使用统计」表格

第 9 步：Reviewer 查看审查报告
  └─ 报告中自动包含每个 commit 的 AI 使用比例表格
     （如果 git-ai 未安装，表格不会出现，但审查报告其余部分完全正常）
```

**验证点：**
- 审查报告末尾有「AI 代码使用统计」表格 = 步骤 8.3/8.4 正常工作
- 审查报告末尾没有该表格但其他内容正常 = git-ai 未安装或无数据，降级成功

---

## 五、实施路线图——按什么顺序做，每步做什么

> **为什么分 Phase？** 每个 Phase 都是独立可交付的，做完一个就能用一个。
> 不需要等全部做完才能看到效果。

### Phase 1（1-2 天）：让 Speckit 安装/更新时同步处理 git-ai

**目标：** 新成员初始化时能补齐 git-ai，已有项目更新 `.specify/` 时能顺带更新 git-ai。

| 步骤 | 具体操作 | 要修改/创建的文件 | 怎么验证做完了 |
|------|---------|------------------|---------------|
| 1.1 | 按本文「需求 1 步骤 1」创建 post-init.ps1 | `.specify/scripts/powershell/post-init.ps1` | 在新目录 clone 仓库后执行脚本，`git-ai --version` 有输出 |
| 1.2 | 按本文「需求 1 步骤 2」修改 batch-update.ps1 | `.specify/scripts/powershell/batch-update.ps1` | 执行 `specify init --here --force --ai copilot --script ps` 后自动出现 `[speckit/post-init]` 日志 |
| 1.3 | 按本文「需求 1 步骤 3」修改 Agent prompt | `.github/agents/speckit.specify.agent.md` | Agent 执行 specify flow 时自动运行 post-init.ps1 |
| 1.4 | （可选）按本文「需求 1 步骤 4」修改 check-prerequisites.ps1 | `.specify/scripts/powershell/check-prerequisites.ps1` | Agent 不触发 Step 0 时，check-prerequisites 也能检测到 git-ai 缺失 |

### Phase 2（2-3 天）：实现主动上传命令

**目标：** 开发者可以用一条命令把 AI 统计数据上传到远程。

| 步骤 | 具体操作 | 要修改/创建的文件 | 怎么验证做完了 |
|------|---------|------------------|---------------|
| 2.1 | 按本文「需求 2 - 3.3」创建 upload-ai-stats.ps1 | `.specify/scripts/powershell/upload-ai-stats.ps1` | `upload-ai-stats.ps1 -DryRun` 能看到 commit 列表和 AI 统计预览 |
| 2.2 | 确认 `git-ai stats <sha> --json` 输出格式满足需求 | 无需修改，只验证 `src/authorship/stats.rs` | 对任意有 AI note 的 commit 执行，JSON 输出包含所有需要的字段 |
| 2.3 | 按本文「3.6」确认默认 remote URL 或显式覆盖，并配置 api_key | 用户环境变量 / CI Secret | 新开 shell 后能读取到 `GIT_AI_REPORT_REMOTE_API_KEY`；如显式写入 URL，则 `GIT_AI_REPORT_REMOTE_URL` 也能读取 |
| 2.4 | 真实上传测试 | 需要远程 API 服务可用 | `upload-ai-stats.ps1` 执行后返回"成功上传" |

### Phase 3（2-3 天）：Code Review 自动上传

**目标：** Code Review 时自动附带 AI 统计数据，reviewer 不需要额外操作。

| 步骤 | 具体操作 | 要修改/创建的文件 | 怎么验证做完了 |
|------|---------|------------------|---------------|
| 3.1 | 按本文「3.4 步骤 1」在 Code Review Agent 追加 8.3/8.4 | `.github/agents/speckit.code-review.agent.md` | Agent 执行 /speckit.code-review 后审查报告末尾有 AI 统计表格 |
| 3.2 | 按本文「3.4 步骤 2」在报告模板追加 AI 统计章节 | `.specify/templates/code-review/template.md` | 报告中表格格式正确，数据与 git-ai stats 输出一致 |
| 3.3 | 测试降级场景 | 无需文件修改 | 卸载 git-ai 后执行 Code Review，报告正常生成但无 AI 统计表格 |

### Phase 4（5-10 天）：远程 API 服务（服务端开发）

**目标：** 搭建接收数据的后端服务和展示仪表盘。

| 步骤 | 具体操作 | 怎么验证做完了 |
|------|---------|---------------|
| 4.1 | 对接 `/api/public/upload/ai-stats` 批量接收端点 | curl 发送本文「3.5」中的 batch JSON，返回 200 且带 `results[]` |
| 4.2 | 设计 MySQL 表，并在服务端按 `commitSha` 做幂等过滤 | 重复 POST 同一个 commit，不会再次插入重复记录 |
| 4.3 | 实现仪表盘页面（按项目、成员、时间维度展示） | 浏览器打开仪表盘 URL 能看到上传的数据 |
| 4.4 | Code Review 文档中嵌入 AI 统计链接 | 审查文档页面能跳转到对应 commit 的 AI 统计详情 |

### Phase 5（持续）：推送 authorship notes 到远端仓库

**目标：** 让远端 git 仓库也存有完整的 authorship notes，作为数据的"终极备份"。

| 步骤 | 具体操作 | 怎么验证做完了 |
|------|---------|---------------|
| 5.1 | 利用 git-ai 已有的 `push_authorship_notes()` 机制 | `git push` 后在远端执行 `git log --notes=ai` 能看到归因数据 |
| 5.2 | 仪表盘可从 git notes 反查逐文件 AI 归因（可选增强） | 仪表盘展示某个 commit 时能 drill-down 到哪些文件的哪些行是 AI 写的 (**客户端侧已实现**：`Get-CommitAiFileStats` 已能解析 authorship note 产出逐文件归因，上传 `stats.files[]`；服务端仪表盘待对接) |

---

## 六、安全与容错——可能出什么问题，怎么处理

> **为什么要单独列这个章节？** 因为系统的可靠性不是"写完能跑"就行了——
> 关键是"出错时不会把其他功能搞挂"。下表列出了每种可能的故障及处理方式。

| 可能出的问题 | 怎么处理 | 为什么这样处理 |
|-------------|---------|---------------|
| **git-ai 安装失败**（网络问题、权限问题） | `post-init.ps1` 打印 Warning 但不报错退出，Speckit 其他功能正常使用 | git-ai 是"锦上添花"，不是 Speckit 的核心依赖，安装失败不应阻塞开发 |
| **upload-ai-stats.ps1 上传失败**（API 不可达） | 一次批量请求失败时整批标记失败；若服务端返回 `results[]`，则按 commit 维度展示"N 成功, M 失败" | 降低请求次数，同时保留按 commit 追踪失败的能力 |
| **Code Review 时 git-ai 未安装** | 步骤 8.3 检测到 `git-ai --version` 失败后，直接跳到步骤 9，审查报告正常生成但没有 AI 统计表格 | 审查报告的核心价值是代码质量问题，AI 统计是附加信息 |
| **某个 commit 没有 AI authorship note** | 仍然调用 `git-ai stats <sha> --json`，但将 `hasAuthorshipNote=false` 且把该 commit 归到 `unknownAdditions` 视图 | 最新 `stats` 已能表达“没有归因 note，但有新增行”的情况，直接跳过会丢失有效数据 |
| **API Key 泄露** | API Key 只存在本机环境变量或 CI Secret，不进入仓库文件 | 密钥不进 git，即使 `.specify/` 被提交也不含敏感信息 |
| **网络超时** | 上传请求设置 10 秒 timeout | 防止长时间挂起，影响开发体验 |
| **代码内容泄露** | 只上传统计数据（行数、比例、工具名），**从不上传代码内容** | 隐私第一：统计数据足够做管理决策，不需要源代码 |
| **重复上传同一个 commit** | 远端 API / 服务端按 `commitSha` 做幂等过滤 | 幂等设计：多次上传不会产生重复记录 |

---

## 七、配置示例——拿去就能用

### 团队统一配置（可选增强，不是当前默认实现）

**如果你们希望把当前默认地址显式写入环境变量，方便排查或兼容其他脚本，可以在 `post-init.ps1` 末尾追加以下内容：**

```powershell
# === 团队统一配置 ===
# 所有成员共用同一个统计上传地址
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_URL", "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats", "User")

# API key 不要写在这里！让每个成员自己配置，或从环境变量读取
# 如果团队有统一的 CI 账号 key，也只建议在 CI Secret 中配置，而不是写入仓库脚本
```

**为什么 remote URL 可以预置但 api_key 不可以？**
- remote URL 是公开的地址，不算敏感信息
- api_key 是身份凭证，写入代码仓库 = 任何能访问仓库的人都能用你的身份上传数据

**当前已验证原型默认做的事情只有两类：**
- 安装 git-ai（默认从 `https://usegitai.com/install.ps1` 下载，也可通过 `GIT_AI_INSTALLER_URL` 覆盖）
- 刷新 `git-ai install-hooks` 集成配置

它**不会**默认写入 `GIT_AI_REPORT_REMOTE_URL`、`GIT_AI_REPORT_REMOTE_ENDPOINT`、`GIT_AI_REPORT_REMOTE_PATH`、`GIT_AI_REPORT_REMOTE_API_KEY` 或 `GIT_AI_REPORT_REMOTE_USER_ID`。这些值目前仍然建议由团队脚本追加，或由成员自行在本机环境变量里配置。

不过，如果这些 URL 相关环境变量都未设置，Windows 下的上传脚本会回退到内置默认地址 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`。

不过，Windows 下的上传脚本现在会在 `GIT_AI_REPORT_REMOTE_USER_ID` 未设置时，继续尝试从本机 VS Code / IDEA 的 MCP 配置中读取 `X-USER-ID`，以便和 Code Review MCP 配置保持一致。

### 成员个人配置

**每个成员在自己的电脑上执行一次：**

```powershell
# 设置个人的 API key（新开 shell 生效）
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_API_KEY", "your-personal-key", "User")
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_USER_ID", "your-user-id", "User")

# 验证配置是否正确
Write-Host $env:GIT_AI_REPORT_REMOTE_URL
# → https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats

Write-Host ($env:GIT_AI_REPORT_REMOTE_API_KEY.Substring(0,4) + "***")
# → your***
```

### CI/CD 环境配置

**在 CI pipeline 中通过环境变量传入（不需要修改 config 文件）：**

```yaml
# GitLab CI 示例
variables:
    GIT_AI_REPORT_REMOTE_URL: "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
  GIT_AI_REPORT_REMOTE_API_KEY: $CI_AI_STATS_KEY  # 从 CI 密钥库读取

# GitHub Actions 示例
env:
    GIT_AI_REPORT_REMOTE_URL: "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
  GIT_AI_REPORT_REMOTE_API_KEY: ${{ secrets.AI_STATS_KEY }}
```

---

## 八、总结——做了什么、为什么这样做、效果是什么

| 需求 | 方案 | 核心修改 | 为什么选这个方案 |
|------|------|----------|-----------------|
| 安装/更新 Speckit 时装或更新 git-ai | `post-init.ps1` + `batch-update.ps1` + Agent prompt Step 0 | 新建 1 个脚本 + 修改 1 个包装脚本 + 修改 1 个 Agent prompt | 不修改 git-ai Rust 代码；对可控包装链路立即生效；裸 `specify init` 需上游 hook 或 wrapper |
| 开发者主动上传 AI 统计 | `upload-ai-stats.ps1` 命令 | 新建 1 个 PowerShell 脚本 | 开发者掌控上传时机，不阻塞 commit，并通过一次批量请求上传多个 commit |
| Code Review 自动上传 | 扩展 Code Review Agent 步骤 8 | 修改 1 个 agent prompt + 1 个报告模板 | 无需 reviewer 额外操作，数据自动附带 |

**三个核心设计原则：**

1. **最小侵入 Spec Kit 源码** ——只在 `specify init` 中增加一个通用的 post-init 执行点；git-ai 的具体安装逻辑仍然放在 PowerShell 脚本层。不改 git-ai Rust 代码。
2. **不阻塞开发流程** ——每次 commit 只在本地写 authorship note（毫秒级），远程上传由用户或 Code Review 主动触发。
3. **降级友好** ——git-ai 未安装时所有功能静默跳过（Warning 提示，不报错），不影响 Speckit 核心流程。

**最终效果：**
- 新成员：clone → `specify init` 自动触发 post-init → 自动安装 git-ai → 正常开发 → AI 使用自动记录
- 开发者：功能做完 → 一条命令上传统计 → leader 在仪表盘看到数据
- Reviewer：Code Review → 自动附带 AI 统计表格 → 无需额外操作

---

## 变更日志

### 2026-04-19：逐文件统计改用 commit-local 语义（authorship note 直接解析）

**变更原因：**

原实现中 `Get-CommitAiFileStats` 调用 `git-ai diff <sha> --json --include-stats` 来获取逐文件的 AI/人工归因。但 `git-ai diff` 是 **provenance-traced** 的——它会跨 commit 追溯每一行的来源。这导致一个纯人工 commit（例如 `039e24e`，21行人工，0行 AI）在逐文件统计中被错误地标记为有 AI 行（因为某些行在更早的 commit 中由 AI 生成过）。

这不符合 commit-local 的业务语义："这个 commit **本身**有多少 AI 参与"。

**变更内容：**

| 项目 | 旧方案 | 新方案 |
|------|--------|--------|
| 逐文件数据来源 | `git-ai diff <sha> --json --include-stats` | `git notes --ref=ai show <sha>` + `git diff-tree --numstat` |
| 语义 | Provenance-traced（跨 commit 追溯行的来源） | Commit-local（只看当前 commit 自身的归因） |
| 函数签名 | `Get-CommitAiFileStats -CommitSha $sha -DiffData $data` | `Get-CommitAiFileStats -CommitSha $sha` |
| `Get-CommitAiStats` 调用方式 | 先调 `git-ai diff`，再把 DiffData 传给文件级函数 | 直接调用 `Get-CommitAiFileStats -CommitSha $sha`（函数内部自行读取 note） |

**新实现要点：**

1. `git diff-tree --no-commit-id --numstat -r <sha>` → 每个文件的新增/删除行数
2. `git notes --ref=ai show <sha>` → 该 commit 的 authorship note 原文
3. 解析 attestation 段（`---` 分隔符之前）：非缩进行=文件路径，缩进行=`<id> <start>-<end>` 归因条目
4. `h_*` 前缀 → 人工行，其他 id → AI prompt hash → 从 JSON 元数据的 `prompts.<hash>.agent_id.tool/.model` 获取工具/模型信息
5. 合并 numstat 行数 + attestation 归因 → `stats.files[]`

**验证结果：**

| 测试 commit | 预期 | 实际 | 状态 |
|------------|------|------|------|
| `039e24e`（21行人工，无 AI） | 每个文件：human=7, AI=0 | human=7, AI=0 | ✅ |
| `d41e130`（13行 AI，AiExtraTest.java） | AI=13, human=0, tool=github-copilot/gpt-4.1 | AI=13, human=0, tool=github-copilot::gpt-4.1 | ✅ |

**影响范围：**

- `_external/spec-kit/scripts/powershell/upload-ai-stats.ps1`（主模板）✅ 已更新
- `_external/spec-kit/.specify/scripts/powershell/upload-ai-stats.ps1`（自举副本）✅ 已同步
- `_external/spec-kit/test-verify/.specify/scripts/powershell/upload-ai-stats.ps1`（验证副本）✅ 已同步
- commit 级统计（`Get-CommitAiStats` 调用 `git-ai stats`）不受影响，仍使用原方案
