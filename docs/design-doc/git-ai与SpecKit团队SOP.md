# 团队 AI 编码归因与统计看板 SOP（Windows / PowerShell）

> 状态：团队使用说明
> 日期：2026-04-27
> 适用环境：Windows 10/11 + PowerShell
> 目标：让团队成员能完成安装、初始化、提交、自检和补传

---

## 背景与方案

### 当前现状（我们为什么需要这个工具）

#### AI 使用管理现状

当前团队已经在大量使用 `GitHub Copilot` 等 AI 工具写代码，但这些使用情况大多停留在个人 IDE、会话记录和成员自报里，缺少统一、持续、可核对的数据视图。管理者通常只能知道“团队在用 AI”，却很难准确回答下面这些问题：

1. 谁在持续使用 AI，使用趋势如何。
2. 哪些提交里 AI 参与度高，哪些提交仍然主要是人工修改。
3. 不同工具、不同模型在团队里的实际使用情况如何。
4. 某次提交里的 AI 代码后来有没有被人工覆盖、修改或删除。
5. 看板上看到的数据，能不能回到具体 commit、文件和行进行核对。

#### 当前核心缺口

现有 Git、代码评审和静态扫描流程，更多解决的是“代码改了什么、有没有明显问题”，并不天然回答“这些代码是谁生成的、由哪个 AI 生成、最终保留了多少”。如果没有提交级归因和统计底座，团队就会长期面临：

1. AI 使用情况主要靠口头汇报，管理口径不稳定。
2. 数据无法按成员、提交和时间维度持续汇总，难以形成可信看板。
3. 出现异常提交或需要复盘时，无法快速下钻到具体 commit、文件、行和来源。
4. 本地有记录、平台没数据时，难以及时发现和补齐。

#### 现状核心总结

当前真正缺的不是再多一个“代码审查能力”，而是一条把 AI 编码行为沉淀成提交级数据、再汇总成看板的基础链路。没有这条链路，管理者看不到团队成员的真实使用情况，研发也拿不到统一的归因和统计依据。

### 本方案可以解决什么问题

本方案的核心价值，是把“AI 写了什么、最终留下了什么、由哪个工具和模型产生、哪些提交已经进入统计视图”这几件事数字化，并直接展示给管理者和研发负责人。具体来说：

1. 让管理者按成员、提交和时间维度查看 AI 使用情况，而不是依赖口头汇报或截图。
2. 让管理者看到每次提交中的 `AI / human / mixed` 数据，以及团队整体趋势。
3. 让研发负责人能够从看板下钻到具体 commit，结合 `git notes`、AI blame 和来源信息做核对与复盘。
4. 让团队基于统一数据判断哪些工具、模型和提示方式更有效，而不是只看主观感受。
5. 当自动上传或本地链路异常时，可以通过自检和补传尽量保证统计结果完整。

### 实现思路与方案

这套方案应按 `git-ai` 官方能力来理解，核心是“归因 -> 统计 -> 查看”，不是“先做代码审查”。`git-ai` 负责把 AI 编码行为记录成可统计、可追溯的数据；`spec-kit` 主要负责把安装、初始化和补传流程标准化，是接入加速器，不是核心能力本身。

整体思路可分为三层：

1. 归因采集层：`git-ai` 在 AI 编辑发生时记录 checkpoint，在 commit 时把结果凝结为 git notes，明确每条提交里哪些行来自哪个 agent、model 和会话来源。
2. 统计展示层：基于 git notes 生成 `git-ai stats`、AI blame 和上传后的平台数据，形成成员、提交、时间范围的统计结果和看板视图。
3. 工程接入层：`spec-kit` 负责统一初始化目录、刷新本地安装、生成补传脚本，降低团队接入和维护成本。

推荐按下面的思路理解整条链路：

```text
AI / 人工修改代码
    -> git-ai checkpoint 记录编辑来源
    -> commit 时写入 git notes
    -> 生成 git-ai stats / AI blame
    -> 上传到平台
    -> 看板查看成员/团队使用情况
    -> 链路异常时再自检和补传
```

当前方案的关键约束也需要提前说明：

1. `git-ai` 不是事后“猜”代码像不像 AI 写的，而是基于编辑时 checkpoint 和 commit 时 git notes 记录来源，所以链路完整性直接决定数据可信度。
2. 管理看板能展示什么，取决于本地是否正确生成了 notes 和 stats，以及上传链路是否完整。
3. 自动化链路一旦失效，如果没有自检和补传机制，平台数据就会持续出现缺口。
4. `spec-kit` 能提升统一落地效率，但不是 `git-ai` 统计和看板能力的前提。

---

## 1. 先看这 4 步

如果你只关心怎么装、怎么用，按下面顺序执行即可：

1. 安装 `specify-cli`。
2. 进入业务仓库。
3. 执行 `specify init`。
4. 正常提交代码，然后用 `git notes` 和 `git-ai stats` 自检。

当前统一使用下面这两条分支：

| 组件 | 仓库 | 分支 | 用途 |
|------|------|------|------|
| `git-ai` | `https://github.com/rj-gaoang/git-ai` | `feature/mcp-x-user-id-hook-payload` | 本地安装、归因、prompt 记录、上传 |
| `spec-kit` | `https://github.com/rj-wangbin6/spec-kit` | `feature_gaoang_2026_04_26` | 初始化 `.specify/`、生成脚本、刷新 `git-ai` |

说明：

1. `specify init` 生成的 `post-init` 会在本机未安装 `git-ai` 时自动下载安装器并完成安装。
2. 团队统一使用仓库 release 产出的 `install.ps1`。

---

## 2. 安装前准备

本机需要具备：

1. Git
2. PowerShell
3. `uv`

先确认环境：

```powershell
git --version
$PSVersionTable.PSVersion
uv --version
```

通过标准：你实际要用到的命令都能返回版本号。

注意：本文所有带 `$Tag`、`$Dir`、`$env:...` 的示例都必须在 PowerShell 里执行，不能在 `cmd.exe` 里执行。快速判断方法：PowerShell 提示符通常类似 `PS C:\work>`，而 `cmd.exe` 通常类似 `C:\work>`。

---

## 3. 安装步骤

### 3.1 安装 `specify-cli`

```powershell
uv tool install specify-cli --force --from git+https://github.com/rj-wangbin6/spec-kit.git@feature_gaoang_2026_04_26
```

验证：

```powershell
specify --version
```

### 3.2 离线安装 `specify-cli`（无法访问 GitHub）

如果成员电脑无法访问 GitHub，`uv tool install specify-cli --from git+https://...` 也会失败。推荐由维护人员先准备 `specify-cli` 离线 wheel 包和依赖包，再分发到内网。

维护人员在能访问外网的机器上准备离线包：

```powershell
$OfflineDir = "D:\spec-kit-offline"
New-Item -ItemType Directory -Force "$OfflineDir\packages" | Out-Null

git clone -b feature_gaoang_2026_04_26 https://github.com/rj-wangbin6/spec-kit.git "$OfflineDir\source"
Set-Location "$OfflineDir\source"

python -m pip wheel --wheel-dir "$OfflineDir\packages" .
Get-ChildItem "$OfflineDir\packages" | Select-Object Name, Length
```

如果维护人员本机已经有对应分支源码，也可以直接进入现有 `spec-kit` 仓库执行：

```powershell
$OfflineDir = "D:\spec-kit-offline"
New-Item -ItemType Directory -Force "$OfflineDir\packages" | Out-Null

Set-Location <你的 spec-kit 仓库路径>
python -m pip wheel --wheel-dir "$OfflineDir\packages" .
```

普通用户拿到 `D:\spec-kit-offline\packages` 后执行：

```powershell
$OfflineDir = "D:\spec-kit-offline"
uv tool install specify-cli --force --find-links "$OfflineDir\packages" --no-index
specify --version
```

通过标准：`specify --version` 能正常返回版本号。

注意：这一步仍要求用户机器已经安装 `uv`。如果用户连 `uv` 也无法联网安装，需要维护人员提前通过内网软件源、企业网盘或安装包把 `uv` 分发到用户机器。

完全离线接入时，推荐顺序是：先按本节安装 `specify-cli`，再按 3.5 离线安装 `git-ai`，最后回到业务仓库执行 `specify init --offline`。这样 `post-init` 会检测到本机已经有 `git-ai`，不会再尝试从 GitHub 下载安装器。

离线初始化业务仓库时，建议显式加 `--offline`，只使用 `specify-cli` 包内置模板，不再回退访问 GitHub：

```powershell
Set-Location <你的业务仓库路径>
specify init . --ai copilot --script ps --offline
```

如果仓库已经初始化过，需要强制刷新：

```powershell
specify init --here --force --ai copilot --script ps --offline
```

### 3.3 安装 `git-ai`

推荐方式不是手工安装，而是在初始化业务仓库时由 `post-init` 自动安装。

进入你的业务仓库后执行：

```powershell
Set-Location <你的业务仓库路径>
specify init . --ai copilot --script ps
```

如果仓库已经初始化过，需要强制刷新：

```powershell
specify init --here --force --ai copilot --script ps
```

这一步会自动完成下面几件事：

1. 生成 `.specify/` 目录。
2. 执行 `.specify/scripts/powershell/post-init.ps1`。
3. 如果本机还没有 `git-ai`，自动下载安装器并安装。
4. 自动执行 `git-ai install-hooks`。
5. 自动把 `prompt_storage` 设成 `notes`。

初始化后确认下面文件存在：

```text
.specify/scripts/powershell/post-init.ps1
.specify/scripts/powershell/upload-ai-stats.ps1
```

再执行下面命令确认 `git-ai` 已经可用：

```powershell
git-ai --version
Get-Command git-ai | Format-List Source
git-ai config prompt_storage
git-ai git-path
```

通过标准：

1. `git-ai --version` 有输出。
2. `Get-Command git-ai` 指向 `%USERPROFILE%\.git-ai\bin\git-ai.exe`。
3. `git-ai config prompt_storage` 返回 `notes`。
4. `git-ai git-path` 能返回底层 Git 路径。

### 3.4 手动安装 `git-ai`（可选）

如需手动安装，直接使用仓库 release 里的 `install.ps1`：

命令格式：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/<owner>/<repo>/releases/download/<tag>/install.ps1 | iex"
```

示例：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/rj-gaoang/git-ai/releases/download/v1.3.5/install.ps1 | iex"
```

如果团队通过 `Spec Kit` 自动安装，也应让 `GIT_AI_INSTALLER_URL` 指向同一个 release 里的 `install.ps1`。

### 3.5 离线安装 `git-ai`（无法访问 GitHub）

如果部分成员电脑无法访问 GitHub，不要让他们直接执行 `irm https://github.com/... | iex`。推荐由维护人员先在能访问 GitHub 的机器上下载离线包，再通过内网共享、企业网盘或 U 盘发给成员安装。

维护人员准备离线包：

```powershell
$Tag = "v1.3.5"
$OfflineDir = "D:\git-ai-offline"
New-Item -ItemType Directory -Force $OfflineDir | Out-Null

Invoke-WebRequest -Uri "https://github.com/rj-gaoang/git-ai/releases/download/$Tag/install.ps1" -OutFile "$OfflineDir\install.ps1" -UseBasicParsing
Invoke-WebRequest -Uri "https://github.com/rj-gaoang/git-ai/releases/download/$Tag/git-ai-windows-x64.exe" -OutFile "$OfflineDir\git-ai-windows-x64.exe" -UseBasicParsing

Get-FileHash "$OfflineDir\install.ps1" -Algorithm SHA256
Get-FileHash "$OfflineDir\git-ai-windows-x64.exe" -Algorithm SHA256
```

说明：

1. 大多数 Windows 电脑使用 `git-ai-windows-x64.exe`。
2. 如果是 ARM64 电脑，把二进制文件换成 `git-ai-windows-arm64.exe`。
3. 分发离线包时建议同时附上维护人员计算出的 SHA256，安装前后方便核对文件是否被替换或传坏。

普通用户拿到离线包后，在 PowerShell 执行：

```powershell
$OfflineDir = "D:\git-ai-offline"
Unblock-File "$OfflineDir\install.ps1" -ErrorAction SilentlyContinue
Unblock-File "$OfflineDir\git-ai-windows-x64.exe" -ErrorAction SilentlyContinue

$env:GIT_AI_LOCAL_BINARY = "$OfflineDir\git-ai-windows-x64.exe"
powershell -NoProfile -ExecutionPolicy Bypass -File "$OfflineDir\install.ps1"
Remove-Item Env:\GIT_AI_LOCAL_BINARY -ErrorAction SilentlyContinue
```

如果担心漏掉前面的环境变量设置，也可以直接复制这一行，在同一个 PowerShell 窗口里一次执行完：

```powershell
$OfflineDir = "D:\git-ai-offline"; $env:GIT_AI_LOCAL_BINARY = "$OfflineDir\git-ai-windows-x64.exe"; powershell -NoProfile -ExecutionPolicy Bypass -File "$OfflineDir\install.ps1"; Remove-Item Env:\GIT_AI_LOCAL_BINARY -ErrorAction SilentlyContinue
```

这个方式的关键是 `GIT_AI_LOCAL_BINARY`：它会让安装器直接复制本地 exe，不再访问 GitHub 下载二进制文件。安装脚本仍会完成下面几件事：

排查方法：如果安装日志显示的是 `Downloading git-ai (repo: ..., release: local)...`，说明已经走本地离线安装分支；如果显示的是 `release: v2.0.5`、`release: latest` 或其他版本号，说明当前这个 PowerShell 进程里没有拿到 `GIT_AI_LOCAL_BINARY`，脚本会继续尝试访问 GitHub 下载。

注意：不要直接把下载下来的 `git-ai-windows-x64.exe` 当成 `git-ai` 命令来运行。`git-ai` 会根据可执行文件名分发入口，只有文件名是 `git-ai` 或 `git-ai.exe` 时，才会进入 `git-ai` 命令模式；原始 release 资产名 `git-ai-windows-x64.exe` 直接执行时，不等价于已经安装完成。

1. 安装到 `%USERPROFILE%\.git-ai\bin`。
2. 生成 `git-ai.exe` 和 `git.exe` shim。
3. 生成 `git-og.cmd`。
4. 配置 PATH。
5. 执行 `git-ai install-hooks`。

如果只是临时验证下载下来的二进制能否运行，可以先复制一份并改名成 `git-ai.exe` 再执行：

```powershell
Copy-Item "$OfflineDir\git-ai-windows-x64.exe" "$OfflineDir\git-ai.exe"
& "$OfflineDir\git-ai.exe" --version
Remove-Item "$OfflineDir\git-ai.exe" -ErrorAction SilentlyContinue
```

但日常使用仍然推荐按上面的离线安装步骤执行，让安装脚本统一处理 PATH、shim 和 hooks。

离线安装后验证：

```powershell
git-ai --version
Get-Command git-ai | Format-List Source
where.exe git-ai
where.exe git
git-ai git-path
git-ai config set prompt_storage notes
git-ai config prompt_storage
```

通过标准：

1. `git-ai --version` 有输出。
2. `Get-Command git-ai` 指向 `%USERPROFILE%\.git-ai\bin\git-ai.exe`。
3. `where.exe git` 第一条是 `%USERPROFILE%\.git-ai\bin\git.exe`，后面才是系统 Git。
4. `git-ai git-path` 能返回真实 Git 路径，不能指回 `.git-ai\bin\git.exe`。
5. `git-ai config prompt_storage` 返回 `notes`。

如果后续还要执行 `specify init --offline`，建议先完成上面的离线安装。这样 `post-init` 会检测到本机已经有 `git-ai`，默认不会再从 GitHub 下载安装器，只会刷新 hooks 和 `prompt_storage`。

如果团队有内网 HTTP 文件服务器，也可以把 `install.ps1` 放到内网地址，并在执行 `specify init` 前设置：

```powershell
$env:GIT_AI_INSTALLER_URL = "http://<内网地址>/git-ai/install.ps1"
$env:GIT_AI_LOCAL_BINARY = "D:\git-ai-offline\git-ai-windows-x64.exe"
```

只有在需要强制重装时才需要这两个环境变量。普通离线用户更推荐先手动安装一次，再执行 `specify init`。

### 3.6 发布新版本（维护人员）

发布步骤：

1. 修改 [git-ai/Cargo.toml](c:/Users/15126/Desktop/ruijie/git-ai-view/git-ai/Cargo.toml) 里的 `version`。
2. 提交并推送到你的 GitHub 仓库。
3. 在 GitHub Actions 里手动运行 `Release Build` 工作流。
4. 发布时把 `dry_run` 设为 `false`。
5. 工作流会自动创建 release，并生成该版本对应的 `install.ps1`。

发布完成后，普通用户就可以直接执行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/<owner>/<repo>/releases/download/<tag>/install.ps1 | iex"
```

---

## 4. 日常怎么用

### 4.1 正常开发和提交

日常开发不需要额外记复杂命令，正常提交即可：

```powershell
git add .
git commit -m "feat: add user login flow"
```

提交后，`git-ai` 会在本地记录这次 commit 的 authorship note 和统计信息。

### 4.2 提交后怎么自检

每次要确认“这次 commit 是否真的记录到了 AI 数据”，执行下面 3 条：

```powershell
git notes --ref=ai list HEAD
git notes --ref=ai show HEAD
git-ai stats HEAD --json
```

通过标准：

1. `git notes --ref=ai list HEAD` 有输出。
2. `git notes --ref=ai show HEAD` 能看到文件归因和 prompt 元数据。
3. `git-ai stats HEAD --json` 返回合法 JSON。

### 4.3 怎么看 prompt

先找 prompt hash：

```powershell
git notes --ref=ai show HEAD
```

然后查看指定 prompt：

```powershell
git-ai show-prompt <promptHash> --commit HEAD
```

注意：

1. `git-ai show-prompt` 不能直接空跑，必须带 `promptHash`。
2. 当前如果 note 里的 `messages` 本身为空，服务端也不会凭空生成 `prompt_text`。

### 4.4 怎么手动补传或批量上传

如果某次 commit 没有自动上传，或者你要补传历史 commit，执行：

```powershell
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits "abc123,def456"
```

常见用法：

```powershell
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits "abc123"
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Since "2026-04-01" -Until "2026-04-14"
.\.specify\scripts\powershell\upload-ai-stats.ps1 -DryRun
```

### 4.5 怎么开启自动上传

如果需要 commit 后自动上传，先配置环境变量：

```powershell
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
$env:GIT_AI_REPORT_REMOTE_USER_ID = "<你的 X-USER-ID>"
$env:GIT_AI_REPORT_REMOTE_API_KEY = "<你的 API Key，可选>"
$env:GIT_AI_DEBUG = "1"
```

如需显式开关自动上传，请使用：

```powershell
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "true"
```

注意：

1. 用 `true/false`，不要用 `1/0`。
2. `GIT_AI_DEBUG=1` 方便排查上传日志。
3. 自动上传依赖 `post-commit` hook 真正生效；如果仓库或全局 `core.hooksPath` 被其他工具接管，需要先确认 `git-ai` 的 hook 已安装到当前实际生效的 hooks 目录。

---

## 5. 自助验证

### 5.1 最小本地验证

如果你想快速确认本机安装是否可用，建议单独建一个测试仓库：

```powershell
New-Item -ItemType Directory -Force C:\work\git-ai-self-verify | Out-Null
Set-Location C:\work\git-ai-self-verify

git init
git branch -M main
git config user.name "git-ai-self-verify"
git config user.email "git-ai-self-verify@example.com"
```

让 AI 生成一小段代码后执行：

```powershell
git add .
git commit -m "test: verify git-ai local install"

git notes --ref=ai show HEAD
git-ai stats HEAD --json
```

### 5.2 自动上传验证

配置好上传环境变量后，再做一次 commit：

```powershell
git add .
git commit -m "test: verify native auto upload"
```

期望本地日志包含类似内容：

```text
[git-ai] upload-ai-stats: uploaded stats for <short_sha>
```

### 5.3 数据库只读验证

如果你有数据库只读权限，可以按 commit SHA 查询：

```sql
SELECT COUNT(*) AS commit_row_count
FROM cr.git_ai_commit_stats
WHERE commit_code = '<完整commit sha>';

SELECT p.id, p.prompt_hash, p.tool, p.model, p.prompt_text
FROM cr.git_ai_prompt_stats p
JOIN cr.git_ai_commit_stats c ON p.commit_id = c.id
WHERE c.commit_code = '<完整commit sha>';
```

通过标准：

1. `git_ai_commit_stats` 能查到这条 commit。
2. `git_ai_prompt_stats` 能查到 prompt 明细。

---

## 6. 更新、重装、卸载

### 6.1 最常用：直接覆盖更新

最常用的更新方式是重新执行一次初始化：

```powershell
Set-Location <你的业务仓库路径>
specify init --here --force --ai copilot --script ps
```

这会重新执行 `post-init`，从而刷新 `git-ai` 安装、hooks 和 `prompt_storage` 配置。

如果你是手动安装的，也可以直接重新执行你仓库 release 里的安装命令：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/<owner>/<repo>/releases/download/<tag>/install.ps1 | iex"
```

### 6.2 手动刷新 hooks

如果你怀疑 hooks 没刷新，可以执行：

```powershell
git-ai install-hooks
```

### 6.3 卸载 `git-ai`

```powershell
git-ai uninstall-hooks
& "$HOME\.git-ai\bin\git-ai.exe" bg shutdown --hard
```

然后删除安装目录和 PATH：

```powershell
$newUserPath = [Environment]::GetEnvironmentVariable('Path', 'User') -split ';' |
    Where-Object { $_ -and $_.Trim() -ne "$HOME\.git-ai\bin" }

[Environment]::SetEnvironmentVariable('Path', ($newUserPath -join ';'), 'User')
Remove-Item -Recurse -Force "$HOME\.git-ai"
```

验证是否卸干净：

```powershell
Get-Command git-ai -ErrorAction SilentlyContinue
Test-Path "$HOME\.git-ai"
```

### 6.4 重新安装 `specify-cli`

卸载：

```powershell
uv tool uninstall specify-cli
```

重新安装：

```powershell
uv tool install specify-cli --force --from git+https://github.com/rj-wangbin6/spec-kit.git@feature_gaoang_2026_04_26
```

---

## 7. 最常见的 7 个问题

### 7.1 `git-ai show-prompt` 报错

原因通常是没有传 `promptHash`。先执行 `git notes --ref=ai show HEAD` 找 hash，再执行 `git-ai show-prompt <promptHash> --commit HEAD`。

### 7.2 初始化后没生效

优先检查：

1. `specify --version` 是否来自当前联调分支。
2. `git-ai --version` 是否仍是旧版本。
3. `git-ai config prompt_storage` 是否为 `notes`。
4. 必要时重新执行 `specify init --here --force --ai copilot --script ps`。

### 7.3 自动上传没反应

优先检查：

1. `GIT_AI_REPORT_REMOTE_URL` 是否正确。
2. `GIT_AI_REPORT_REMOTE_USER_ID` 是否已配置。
3. `GIT_AI_DEBUG=1` 是否已开启。
4. `GIT_AI_AUTO_UPLOAD_AI_STATS` 是否被写成了 `1`，正确值应为 `true`。

### 7.4 安装时报 `Could not detect a standard git binary on PATH`

这个报错的含义不是 `git-ai` 下载失败，而是安装器在当前 PowerShell 进程里没有找到“真实 Git”。安装器只接受标准 Git 可执行文件，不接受 `%USERPROFILE%\.git-ai\bin\git.exe` 这种 `git-ai` 自己生成的 shim。旧版 Windows 安装器还有一个限制：它优先只看 PATH 里的第一个 `git.exe`，如果第一个刚好是 `git-ai` shim，就可能在真实 Git 明明还在 PATH 后面的情况下也误报这个错误。

先执行下面几条检查：

```powershell
git --version
Get-Command git.exe | Format-List Path, Source
where.exe git
```

常见原因通常是下面两类：

1. 这台机器根本没安装 Git，或者 Git 安装了但没有加入 PATH。
2. PATH 里先命中了 `%USERPROFILE%\.git-ai\bin\git.exe`，而旧版安装器没有继续往后找真实 Git；如果 `%USERPROFILE%\.git-ai\config.json` 里也没有保存有效的 `git_path`，就会直接失败。

如果你确认系统已经安装了 Git for Windows，最短修复方式是在当前 PowerShell 会话里先把真实 Git 临时放到 PATH 最前面，再重跑安装器。默认安装路径通常是：

```powershell
$env:PATH = "C:\Program Files\Git\cmd;$env:PATH"
git --version
```

然后重新执行安装命令。

如果你的 Git 不在默认目录，先用资源管理器或开始菜单确认真实安装路径，再把对应目录替换到上面的 `C:\Program Files\Git\cmd`。

如果 `where.exe git` 只能看到 `%USERPROFILE%\.git-ai\bin\git.exe`，看不到真实 Git，可优先尝试：关闭当前 PowerShell 窗口，重新打开一个新 PowerShell，再执行上面的检查和安装命令。

### 7.5 `git-ai --version` 没输出，而且路径是 `C:\Users\admin.git-ai\bin`

如果 `Get-Command git-ai` 或 `where.exe git-ai` 显示的是下面这种路径：

```text
C:\Users\admin.git-ai\bin\git-ai.exe
```

这不是标准安装目录。标准目录应该是：

```text
C:\Users\admin\.git-ai\bin\git-ai.exe
```

区别是 `admin` 后面少了一个反斜杠。PowerShell 里 `"$HOME.git-ai\bin"` 会展开成 `C:\Users\admin.git-ai\bin`，不是 `C:\Users\admin\.git-ai\bin`。正确写法必须是 `"$HOME\.git-ai\bin"`，或者使用 `Join-Path $HOME ".git-ai\bin"`。

先执行下面命令确认当前命中了哪个路径：

```powershell
Get-Command git-ai | Format-List Name, Source, Path, Definition
where.exe git-ai
where.exe git
```

如果确认命中了 `C:\Users\admin.git-ai\bin`，按下面方式清理错误 PATH 和错误目录，然后重新安装：

```powershell
$WrongBin = "$HOME.git-ai\bin"

$newUserPath = [Environment]::GetEnvironmentVariable('Path', 'User') -split ';' |
    Where-Object { $_ -and $_.Trim() -ne $WrongBin }
[Environment]::SetEnvironmentVariable('Path', ($newUserPath -join ';'), 'User')

$newProcessPath = $env:PATH -split ';' |
    Where-Object { $_ -and $_.Trim() -ne $WrongBin }
$env:PATH = ($newProcessPath -join ';')

Remove-Item -Recurse -Force "$HOME.git-ai" -ErrorAction SilentlyContinue

$env:PATH = "C:\Program Files\Git\cmd;$env:PATH"
$OfflineDir = "D:\git-ai-offline"
$env:GIT_AI_LOCAL_BINARY = "$OfflineDir\git-ai-windows-x64.exe"
powershell -NoProfile -ExecutionPolicy Bypass -File "$OfflineDir\install.ps1"
Remove-Item Env:\GIT_AI_LOCAL_BINARY -ErrorAction SilentlyContinue
```

重开一个 PowerShell 后验证：

```powershell
Get-Command git-ai | Format-List Source
where.exe git-ai
git-ai --version
```

通过标准：`Get-Command git-ai` 必须指向 `%USERPROFILE%\.git-ai\bin\git-ai.exe`，不能再指向类似 `C:\Users\admin.git-ai\bin\git-ai.exe` 的错误目录。

### 7.6 报 `由于找不到 VCRUNTIME140.dll，无法继续执行代码`

这个报错发生在程序真正启动之前，含义是：当前这份 Windows 可执行文件依赖 Microsoft Visual C++ 运行库，但用户机器上没有对应的运行时。常见于旧版 Windows release 是按 `x86_64-pc-windows-msvc` 或 `aarch64-pc-windows-msvc` 动态 CRT 方式构建时。

它不是 `git-ai` 命令逻辑报错，也不是重新执行同一个安装命令就一定能解决的问题。只要这份 exe 还依赖 VC++ 运行库，而机器上又缺这个运行库，程序就会在启动前直接失败。

处理方式分两类：

1. 立即恢复使用：在用户机器安装与架构匹配的 Microsoft Visual C++ Redistributable。
2. 从源头规避：改用新的 Windows release。新版 release 应使用静态 CRT 构建，这样普通用户机器不需要额外安装 `VCRUNTIME140.dll` 对应运行库。

如果是内网离线分发场景，而当前手上的还是旧版 exe，维护人员需要把 VC++ Redistributable 一起打包分发；只分发 `git-ai-windows-x64.exe` 本身还不够。

排查时可以先看这两个点：

```powershell
Get-Command git-ai | Format-List Source
where.exe git-ai
```

如果路径已经正确，但双击或命令行启动时立即弹 `VCRUNTIME140.dll` 缺失，这就不是 PATH 问题，而是系统运行库缺失。

### 7.7 运行时报 `Fatal: Could not locate a real 'git' binary`

这条报错和安装阶段的 `Could not detect a standard git binary on PATH` 很像，但触发位置不同：这是 `git-ai` 可执行文件已经启动了，只是在运行时解析“底层真实 Git”失败了。

旧版行为有两个限制：

1. 优先依赖 `%USERPROFILE%\.git-ai\config.json` 里的 `git_path`。
2. 如果配置里没有有效 `git_path`，只检查少数固定目录；某些机器虽然 `where.exe git` 能找到 `C:\Program Files\Git\cmd\git.exe`，旧版运行时也可能仍然报错。

新版行为已经修正为：

1. 先读取 `config.json` 里的 `git_path`。
2. 再遍历当前 PATH，自动跳过 `%USERPROFILE%\.git-ai\bin\git.exe` 这种 `git-ai` shim。
3. 最后再回退检查常见 Git 安装目录，包括 `C:\Program Files\Git\cmd\git.exe`。

如果你当前用的还是旧版，可先用下面命令临时修复：

```powershell
$env:PATH = "C:\Program Files\Git\cmd;$env:PATH"
git --version
```

或者直接把真实 Git 写进配置文件：

```powershell
git-ai config set git_path "C:\Program Files\Git\cmd\git.exe"
git-ai git-path
```

如果 `git-ai config set git_path ...` 本身也跑不起来，就手工确认 `%USERPROFILE%\.git-ai\config.json` 里是否存在合法的 `git_path`，并确保它指向真实 Git，而不是 `%USERPROFILE%\.git-ai\bin\git.exe`。

---

## 8. 参考文档

如需进一步看设计细节或联调记录，再看下面这些文档：

1. [git-ai看板方案.md](git-ai看板方案.md)
2. [git-ai验证.md](git-ai验证.md)
3. [git-ai/README.md](../../../git-ai/README.md)
4. [spec-kit/README.md](../../../spec-kit/README.md)
