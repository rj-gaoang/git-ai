# git-ai × Speckit 集成与原生上传方案（详细实施指南）

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

**需求 1：** 团队成员安装或更新 Speckit 时，自动安装或更新 git-ai 到目标最新版本，并配置好 hooks。  
**需求 2：** 提供一套完整的 AI 统计上传能力：
- 2A. `git-ai` 在 `post_commit` 里原生、可选地即时上传当前 commit，并自动携带本地失败 / 未上传的 note 记录一起补传
- 2B. `git-ai upload-stats` 提供原生主动上传入口，显式上传本地已有 note/stat 的 commit；如果 commit 缺失 authorship note，也按 metadata-only 口径上传未知归因记录
- 2C. Speckit 继续提供 `upload-ai-stats.ps1` 做手动批量 / 回补上传
- 2D. Code Review 时继续自动补传并在审查报告中展示

> **文档更新说明（2026-04-24）**：本文最初主要从“Speckit 如何集成 git-ai”的视角写，当前已经补充 `git-ai` 本身的源码改动，包括 `post_commit` 挂接点、原生上传模块、feature flag、环境变量约定、失败降级策略，以及代码级验证结果。也就是说，这份文档现在同时覆盖 Speckit 侧改造和 git-ai 侧改造，不再只是一份脚本集成方案。

### 2026-06-29：过滤 IDE 纯格式化改动，避免格式化行进入 unknown

现场问题：`D:\rj-ltc\rj-ltc-contract-web` 在 `2026-06-29 18:03:28` 的提交 `e892a067b77795063fb5c0d5168f0bf815befb61` 已正常进入 git-ai，也成功上传一次；本地 `post_commit_stats_computed` 已经算出 `gitDiffAddedLines=791`、`aiAdditions=713`、`unknownAdditions=78`。这 78 行全部来自 7 个 `src/views/ContractManagement/ContractParsed*` 文件，diff 内容只是把同一行 `<el-option ... />` 展开成多行，属于 IDE/格式化器产生的纯格式化改动。

本次修复不是把 unknown 硬改成 AI 或人工，而是在 stats 口径中过滤“纯格式化 hunk”：同一个 diff hunk 必须同时有删除和新增，并且删除内容、增加内容去掉所有空白字符后完全一致，才视为 format-only。满足条件的 hunk 不计入 `gitDiffAddedLines` / `gitDiffDeletedLines`，也不会进入 `unknownAdditions`。真实新增字段、方法、属性、字符串内容变化不会被过滤。

代码变更：

| 文件 | 变更点 | 影响 |
|------|--------|------|
| `src/authorship/stats.rs` | 新增 `effective_diff_line_stats_by_file` / `effective_diff_line_stats_from_hunks`，统一过滤 ignored file 和 format-only hunk | `git-ai stats`、post-commit 统计、无 note metadata-only 统计都使用过滤后的有效 diff 行数 |
| `src/integration/upload_stats.rs` | 上传 payload 的 `stats.files[]` 改用同一套过滤后的逐文件 numstat；attestation 交集也使用过滤后的 added lines | commit 总数和逐文件明细保持一致，不再出现总数过滤但文件里仍显示 unknown 的情况 |

现场验证：对 `e892a067` 复算 diff，原始为 `rawAdded=791 rawDeleted=215`；过滤后为 `effectiveAdded=713 effectiveDeleted=202`；被过滤的 `filteredAdded=78` 正好等于原本的 unknown。7 个被过滤文件分别为 `ContractParsed.vue` 12 行、`ContractParsed2.vue` 12 行、`ContractParsedInternational.vue` 12 行、`ContractParsedReview.vue` 12 行、`ContractParsedReviewDialog.vue` 6 行、`ContractParsedReviewDomesticHistoryDetail.vue` 12 行、`ContractParsedReviewStandardDetail.vue` 12 行。

历史数据口径：客户端修复只影响后续重算/上传。已经上传到远程看板的旧记录不会自动变化；如果要让 `e892a067` 的远程记录体现新口径，需要用新版本重新计算并显式重传该 commit。

### 2026-06-29: pasted/manual checkpoint additions and early no-note upload

Incidents:

- Sheng Songtao `ai-cr-manage-service` commit `8374de632504b50be70c837a8a34ca891a8f4cec` at `2026-06-29 18:47:48`: wrapper fallback upload waited 5 seconds, did not see the authorship note, and uploaded `hasAuthorshipNote=false` with `gitDiffAddedLines=7239` and `unknownAdditions=7239`. The post-commit note was written later at `18:48:04`, so the bad unknown count was caused by upload timing, not dashboard math.
- Sheng Songtao commit `4af8272d3488ae1ef37162ddaff6db8243d0473d` at `2026-06-29 18:57:19`: the note existed, but local stats still had `unknownAdditions=101`. Logs showed mixed AI files plus human/manual copied files. The old manual gap-fill only handled pure legacy-human commits, not mixed AI + manual commits.
- Longyu `ai-sandbox-project` commit `05b56d83f71f2d0c24c1b016170c09aeb59d9ef5` at `2026-06-29 20:03:17`: the user confirmed a large direct copy/paste. Logs showed `allAddedLineCount=1295`, `pathspecAddedLineCount=553`, `missingAddedLineCount=742`; excluded files had only `human` / `known_human` evidence and no AI path evidence, so the old logic left those pasted additions as unknown.
- Huang Jinzhao `ai-rag-service-java` commit `e8e8d92d9ff1b687f959a9fef7a29fb7658c7460` at `2026-06-29 22:52:22`: the note existed and both wrapper/auto uploads used `hasAuthorshipNote=true`, but local stats were already `gitDiffAddedLines=2484`, `aiAdditions=2303`, `humanAdditions=6`, `unknownAdditions=175`. The 175 unknown lines matched files excluded with `excluded_no_ai_path_evidence` that had only `human=1`.
- Huang Jinzhao `ai-rag-service-python` commit `e2f051de40fbda9192209ea3783ef9f5da3f68f3` at `2026-06-29 22:53:16`: the note existed and upload used `hasAuthorshipNote=true`, but local stats were already `gitDiffAddedLines=644`, `aiAdditions=354`, `humanAdditions=0`, `unknownAdditions=290`. The 290 unknown lines matched six files that had human checkpoints and no AI path evidence.

Root causes:

1. `wrapper_post_commit` fallback upload continued with no-note upload after note wait timeout. For large commits, daemon post-commit note generation can take longer than 5 seconds, so the remote dashboard could receive an all-unknown `hasAuthorshipNote=false` payload before the correct note payload.
2. Post-commit human gap-fill was too narrow. It did not fill unattested added lines for files that had explicit human/known_human checkpoints inside a mixed AI + manual commit.

Fix:

| File | Change | Impact |
| --- | --- | --- |
| `src/commands/git_handlers.rs` | `wrapper_post_commit` fallback upload now passes `--skip-if-authorship-note-missing-after-wait` | If the daemon is running but note generation is slow, fallback upload skips instead of uploading no-note all-unknown data |
| `src/commands/git_ai_handlers.rs` | Added `upload-stats --skip-if-authorship-note-missing-after-wait` | The new skip behavior is opt-in; manual upload and daemon-unavailable paths keep the existing no-note semantics |
| `src/authorship/post_commit.rs` | Added `fill_observed_human_gaps_for_commit` | Added lines on files with explicit human/known_human checkpoints and no AI path evidence are filled as `h_*` human; existing AI/human attestations are never overwritten |

Audit boundary: this does not convert every unknown line to human. A line is filled as human only when its file has observed human evidence and no AI path evidence. AI-evidence files still use AI attribution and AI gap-fill rules.

Validation: ran `cargo fmt --check` and `git diff --check -- src/commands/git_handlers.rs src/commands/git_ai_handlers.rs src/authorship/post_commit.rs`. Broad tests were intentionally not run to avoid recreating a large `target` directory on this machine.

### 2026-06-26：黄芳 6/26 提交未进入远程看板

现场反馈为“用户已配置 MCP，但 2026-06-26 提交的代码归因都没有上传到远程看板”。这次日志形态和 MCP 身份缺失不同：`logs/黄芳-20260626.jsonl` 整份日志里 `post_commit_started=0`、`post_commit_fallback_upload_spawned=0`、`upload_stats_ready=0`、`upload_stats_succeeded=0`、`upload_stats_failed=0`，说明客户端没有发起任何一次提交统计上传，MCP 的 `X-USER-ID` 解析还没有被执行到。

关键提交链路证据集中在 2026-06-26 16:57:45：

| 日志行 | 证据 | 判断 |
|------|------|------|
| 5690 | `git commit --quiet` 在 `D:\project\sale-ai-analyzer\sale-ai-analyzer-service` 发起，`currentExe=C:\Users\admin\.git-ai\launcher\git.exe`，`gitAiVersion=2.2.44` | 真正拦截 commit 的是旧 `launcher\git.exe` |
| 5697-5699 | commit 走 `route=daemon_wrapper`，真实 Git 子进程已启动，`wrapperInvocationId=9591fb25-adb8-4e39-85f4-5978393b3d26` | wrapper 前半段存在，但后续没有本次 commit 的 child exit / post-commit 收口事件 |
| 全量版本分布 | `launcher\git.exe` 共 865 条为 `2.2.44`；同一日志里 `launcher\git-ai.exe` / `bin\git-ai.exe` 已是 `2.2.46` | 安装后出现入口版本分裂：`git-ai.exe` 已更新，PATH 首位的 `git.exe` 仍旧 |

根因是 Windows 安装 / 自修复边界没有把 `launcher\git.exe` 与权威 `launcher\git-ai.exe` 同步。现行入口策略把 `.git-ai\launcher` 放在 PATH 前面，日志也证明 `pathHead[0]=C:\Users\admin\.git-ai\launcher`；但安装脚本只同步了 `bin\git.exe`，没有刷新 `launcher\git.exe`。同时 `install-hooks` 的 `repair_git_proxy_entrypoint` 遇到已有 `git.exe` 会直接返回，旧代理即使存在也不会被新版 `git-ai.exe install-hooks` 覆盖。因此 commit 继续由 2.2.44 的旧代理处理，提供日志里没有进入 2.2.46 的 post-commit fallback upload / upload-stats 链路。排除项：这不是 MCP 配置错误、不是 HTTP / 后端失败、不是 activity lock 阻塞，也不是 stats 计算失败；这些路径的 debug 事件在本日志中均未出现。

本次代码修复：

| 文件 | 变更点 | 影响 |
|------|--------|------|
| `install.ps1` | 新增 `$launcherGitShim`，安装时从 `launcher\git-ai.exe` 同步到 `launcher\git.exe`，并继续同步 `bin\git-ai.exe` / `bin\git.exe` | PATH 命中 launcher 时，`git` 与 `git-ai` 保持同一版本 |
| `install.ps1` | PATH 更新恢复为优先加入 `$launcherDir`，成功文案同时说明 launcher / bin 均已同步 | 避免 launcher / bin 入口策略再分裂 |
| `src/commands/install_hooks.rs` | Windows `repair_git_proxy_entrypoint` 在通过 `git-ai.exe` 运行时会刷新同目录已有 `git.exe`，通过 `git.exe` 自身运行时仍避免覆盖正在执行的代理 | 安装脚本之外，用户手动 `git-ai install-hooks` 也能修复旧 `launcher\git.exe` |
| `tests/windows_script_checks.rs` | 真实 PowerShell 安装测试断言 `launcher\git.exe`、`bin\git.exe` 都与 `launcher\git-ai.exe` 字节一致；新用户创建代理、老用户刷新代理 | 锁定“旧 git proxy 留在 PATH 首位导致 commit 不上传”的回归 |

历史数据口径：这次修复保证后续 commit 会进入新版 post-commit / fallback upload 链路；已经发生但日志中没有完成 post-commit 收口的 2026-06-26 提交，客户端不会凭空补造远程记录。需要历史修复时，应先确认目标 commit SHA 和本地 `refs/notes/ai` 是否存在，再用修复后的 `git-ai upload-stats <sha> --source manual` 或服务端 backfill 做显式补传。

验证记录：

| 命令 | 结论 |
|------|------|
| `cargo test --lib windows_git_proxy_entrypoint -- --nocapture` | 通过，覆盖旧 `git.exe` 已存在时刷新 / 不刷新两种分支 |
| `cargo test --test windows_script_checks windows_install_script_synchronizes_launcher_current_exe_and_compat_bin -- --nocapture` | 通过，隔离 HOME 下真实执行 `install.ps1` 并验证 launcher / bin 四个入口同步 |
| `cargo test --test windows_script_checks windows_install_script_installs_proxy_for_new_users -- --nocapture` | 通过 |
| `cargo test --test windows_script_checks windows_install_script_refreshes_wrapper_for_existing_users -- --nocapture` | 通过 |
| `cargo test --test windows_script_checks windows_install_script_writes_current_exe_pointer_to_launcher -- --nocapture` | 通过 |

### 2026-06-26：龙宇 6/26 提交已上传但全部 unknown

现场反馈为“用户在 2026-06-26 提交的代码都没有归因，既没有识别出来是 AI 写的，也没有识别出来是人工写的”。本次先按用户要求清理排查样本：`logs/龙宇-20260626.jsonl` 已删除 `2026-06-26` 之前的数据，过滤前备份为 `logs/龙宇-20260626.jsonl.bak-before-20260626-filter`，严格日期二次过滤前备份为 `logs/龙宇-20260626.jsonl.bak-before-20260626-strict-filter`，当前保留 5608 行，时间范围为 `2026-06-26T07:32:53+08:00` 到 `2026-06-26T19:00:01+08:00`。

这次不是“完全没上传”，而是“缺 authorship note 后 fallback 上传了 metadata-only 记录”。当天日志里 5 次 `git commit` 都由旧入口 `C:\Users\admin\.git-ai\launcher\git.exe`、`gitAiVersion=2.2.44` 拦截；第一条 `15:34:27` 的 commit 子进程退出码为 1，没有形成提交。后面 4 条提交成功并触发 fallback upload：

| 时间 | commit | 上传统计 | 判断 |
|------|--------|----------|------|
| 15:34:34 | `294d7e29e52efe1a1489d17919777693932f1fee` | `gitDiffAddedLines=2709`、`aiAdditions=0`、`humanAdditions=0`、`unknownAdditions=2709` | `hasAuthorshipNote=false` |
| 15:55:01 | `d26c5e7a024795d1ab35adee9c5fd502e49b8c72` | `20 / 0 / 0 / 20` | `hasAuthorshipNote=false` |
| 16:10:45 | `56fb0e623651b0ed836538c03cc6a9cbd1761e3c` | `4 / 0 / 0 / 4` | `hasAuthorshipNote=false` |
| 16:17:41 | `6bdbce272fbac7f2ea55a1908e9c8403124b7038` | `12 / 0 / 0 / 12` | `hasAuthorshipNote=false` |

关键事件链是：4 次成功 commit 后都有 `post_commit_fallback_upload_spawned(source=wrapper_post_commit)`，随后 `upload_stats_wait_for_authorship_note_finished(foundAuthorshipNote=false)`，等待约 5 秒仍找不到 `refs/notes/ai` note，于是进入 `upload_stats_manual_missing_note_stats_computed`，最终 `upload_stats_succeeded(statusCode=200)`。同一日志里 `post_commit_started=0`、`post_commit_working_log_loaded=0`、`post_commit_authorship_note_written=0`，说明 daemon/post-commit 从未把 checkpoint 工作日志收口成 authorship note。`upload_stats_ready` 还显示 `hasUserId=true/userIdSource=ide_mcp_config`，所以 MCP 用户身份可用；HTTP 200 也排除了远程接口失败。

根因与黄芳场景同属 Windows launcher 入口版本分裂，但表现不同：黄芳是旧 `launcher\git.exe` 让提交完全没有进入新版上传链路；龙宇这里旧 `launcher\git.exe` 2.2.44 拦截 commit，同时新版 `launcher\git-ai.exe` / daemon / `upload-stats` 2.2.46 仍在运行，形成新旧 runtime 混跑。checkpoint 事件本身存在，但提交收口依赖的 post-commit authorship note 没生成；fallback upload 按审计语义不能把缺 note 的新增行硬判为 AI 或人工，只能把 4 个 commit 作为 metadata-only 上传为 `unknownAdditions`。排除项：不是 MCP 配置问题、不是 HTTP/后端失败、不是 stats 计算错误，也不是完全没有 Copilot/checkpoint 事件。

修复口径继续沿用本节上方的 2.46/2.47 Windows 入口同步修复：安装时把 `.git-ai\launcher\git.exe`、`.git-ai\bin\git.exe` 都从权威 `.git-ai\launcher\git-ai.exe` 同步；通过新版 `git-ai.exe install-hooks` 运行时，也会刷新同目录旧 `git.exe`。龙宇机器只要升级到包含该修复的版本并重新执行安装 / `git-ai install-hooks`，后续 commit 会避免 `git.exe` 2.2.44 与 `git-ai.exe` 2.2.46 混跑。历史 4 条 metadata-only 远程记录不会被客户端自动改写；只有在本地仍能找到对应 `refs/notes/ai` 时，才可以显式执行 `git-ai upload-stats <sha> --source manual` 重传，否则不能凭空恢复 AI / 人工归因。

### 2026-06-26：徐建丰 6/25-6/26 看板按人/部门缺记录排查

现场反馈为“6 月 25 日、6 月 26 日提交的代码归因没有上传到远程看板”。本次排查先把“是否发起上传”和“看板是否能按用户/部门归属展示”拆开：

| 日期 | 日志证据 | 判断 |
|------|----------|------|
| 2026-06-25 | `logs/建丰-20260626.jsonl` 中 `1b5f85778617e9b90bfc0a99ea9471a1b915299d`、`dbbc03a180208a9d28ca44602c7ab9b9a1488aad`、`e1e9edbdd4cbb7a383aa48db5702c6f2fd743c1f` 均有 `post_commit_started`、`post_commit_stats_computed` / `upload_stats_payload_build_succeeded`、`upload_stats_succeeded(statusCode=200)` | 不是本地归因未生成，也不是 HTTP 上传失败；这些记录已经以 `source=auto` 发到远程 |
| 2026-06-25 身份字段 | 三次 `upload_stats_ready` 均为 `hasUserId=false`、`userIdSource=missing` | 自动上传没有携带 `X-USER-ID`，如果后端无法仅靠 Git author email 映射用户/部门，看板按人或部门查询会像“没上传” |
| 2026-06-26 | 当前提供日志中 `post_commit_started=0`、`upload_stats_*` 事件数为 0；只有 checkpoint / blame / install-hooks / install test skip 等事件 | 从这份日志不能证明 6/26 发生了提交上传失败；需要目标 commit SHA 或包含提交时刻的 debug 日志继续核对 |

根因在客户端 MCP 身份解析边界：`src/integration/ide_mcp.rs` 旧逻辑只扫描仓库 `.vscode/mcp.json` 和 JSON 顶层 `servers`，而现场常见配置在工作区 `.mcp.json`：

```json
{
  "mcpServers": {
    "codereview-mcp-server": {
      "url": "http://mcppage.ruijie.com.cn:9810/mcp",
      "type": "http",
      "headers": { "X-USER-ID": "108" }
    }
  }
}
```

因此 commit 自动上传阶段没有解析到 `X-USER-ID=108`，`upload_stats_ready` 只能记录 `hasUserId=false/userIdSource=missing`。这次排除的方向包括：post-commit 没执行、stats 没算出、authorship note 缺失、HTTP/network 失败、服务端业务响应拒绝 2026-06-25 payload；这些在 6/25 三个 commit 的日志里都不成立。6/26 则缺少 post-commit/upload 事件，不能和 6/25 混成同一个“上传失败”。

本次代码修复：

| 文件 | 变更点 | 影响 |
|------|--------|------|
| `src/integration/ide_mcp.rs` | 候选路径从仓库根开始向父级工作区查找 `.mcp.json` 和 `.vscode/mcp.json`，仍保留显式环境变量最高优先级 | 子仓库提交时也能读到父工作区 MCP 配置 |
| `src/integration/ide_mcp.rs` | MCP JSON 同时支持顶层 `servers` 与 `mcpServers`，并继续支持 `headers.X-USER-ID` / `requestInit.headers.X-USER-ID` | 兼容 Claude / VS Code / Cursor 常见 `.mcp.json` 结构，未来自动上传会携带 `X-USER-ID` |

历史数据口径：客户端修复只防止后续提交继续缺 `X-USER-ID`。6/25 三个 commit 已经在服务端创建过记录，后端当前会按 commit code 去重；如果需要让历史记录补上用户/部门维度，不能只让新版客户端重新上传，需要服务端侧 backfill、删除后重传、或把去重创建改为可更新 `x_user_id` 的 upsert/repair 流程。看板查询时还要注意 `source=auto` 需要落在 `source=all` 或包含自动上传的过滤条件中。

验证记录：

| 命令 | 结论 |
|------|------|
| `cargo fmt --check` | 通过 |
| `cargo test --lib integration::ide_mcp -- --nocapture` | 曾在父级工作区查找补充前通过；补充父级查找后本机 Cargo 进程在 5 分钟超时，未拿到最终测试输出，需在空闲环境重跑 |

### 2026-06-29：龙宇 6/27 `rj-ltc-web` 提交绕过 post-commit 后未传后台

现场反馈为“`D:\rj-ltc\rj-ltc-web` 当前分支下，2026-06-27 18:05 / 18:42 有两次 commit 记录，但代码归因未上传”。本次排查以 `logs/龙宇-20260628.jsonl` 为现场证据，不以用户本机另一个 clone `D:\rj-ltc\rj-ltc-web` 作为根因依据。日志里的真实现场路径是 `D:\develop\ltc_project\rj-ltc-web`，git-ai 版本为 `2.2.47`。

关键证据链：

| 时间 / 证据 | 结论 |
|------|------|
| 18:05 附近，`rj-ltc-web` 没有 `command=commit`、`post_commit_started`、`post_commit_stats_computed`、`upload_stats_ready/succeeded/failed` | 目标提交没有进入 git-ai 正常 commit 收口、note 生成和上传链路 |
| 18:23-18:41 多条 checkpoint 的 `baseCommit=199dfcdac2ec481c47821c7108501cf7c9834208`，且包含 `ai_agent` / `known_human` | HEAD 已经推进到 18:05 的 commit，git-ai 后续确实观察到了这个新 base，也仍在正常采集 AI checkpoint |
| 18:42:55-18:43:12，`rj-ltc-web` 只有 IDE 触发的 `reflog` / `tag -l` 等 git 查询事件，没有 commit/post-commit/upload | 第二次提交后同样只被后续 Git 查询观察到，没有触发 post-commit 收口 |
| 同一日志中 `ltc-platform`、`codereivew-upload-mcp` 有 `upload_stats_succeeded(statusCode=200)`，并且 `upload_stats_ready` 显示 `hasUserId=true/userIdSource=ide_mcp_config` | MCP 用户身份、HTTP 上传接口和服务端可达性不是本次根因 |

已能证明的直接原因是：这两次 `rj-ltc-web` commit 创建动作没有进入 git-ai 的正常 `command=commit` 代理收口，因此没有生成 `refs/notes/ai`，也就没有进入自动上传。旧代码只有目标仓库 push 前的 `backfill_missing_authorship_before_push` 兜底；现场 18:05 附近的 push/backfill 是 `D:\develop\ltc_project\ltc-platform`，不是 `rj-ltc-web`，所以不能覆盖目标仓库这两次漏收口提交。

现有日志不能证明“为什么 IDE commit 没进入 git-ai wrapper”的具体机制。git-ai 只能记录已经进入 `C:\Users\admin\.git-ai\launcher\git.exe` 的进程；如果 IDE 使用内置 Git/JGit/libgit2、配置了未包装的 git.exe，或其他工具直接创建 commit，当前日志不会留下该创建进程的 argv/父进程/真实路径。后续要定位这一层根因，需要检查 IDE Git executable 配置，或新增安装/运行诊断去捕获未来 commit 使用的 Git 路径。

排除项：不是 `D:\rj-ltc\rj-ltc-web` 本地 clone 状态导致的判断；不是 MCP / `X-USER-ID` 缺失；不是 HTTP / 后端失败；不是 Copilot checkpoint 完全失效；也不是 2.2.44 / 2.2.46 launcher 新旧混跑问题。本次坏点在“commit 创建没有进入 git-ai commit 收口”；具体绕过原因仍需额外诊断确认。

处理决策：不引入“后续 checkpoint / 普通 Git 查询观察到新 HEAD 后自动补 note/upload”的补救措施。该类补救已按用户要求从 `src/daemon.rs`、`src/git/sync_authorship.rs` 和相关测试中移除，避免在用户写代码或 IDE 查询过程中触发额外归因物化/上传。保留现有正常 post-commit、wrapper fallback upload、push 前 backfill 等既有链路。

2026-06-30 更新：repo-local `post-commit` hook bridge / `git-ai git-hooks install-post-commit` 方案也已按用户要求从未提交代码中撤回。后续对 TortoiseGit / 外部提交工具绕过 wrapper 的处理，需要先补诊断或重新设计方案，不能沿用该实现。

历史数据口径：已经发生且远端缺失的历史记录不会因为客户端升级自动出现在看板。若需要修复历史数据，必须走显式、可审计的 repair/backfill 操作，并以本地仍保留的 checkpoint / working log / authorship note 证据为准；若本地证据已经被清理，则不能凭空恢复 AI / 人工归因，只能按 `hasAuthorshipNote=false` / `unknownAdditions` 的审计口径处理。

验证记录：

| 命令 | 结论 |
|------|------|
| `cargo fmt --check`、`git diff --check`、`cargo check --tests` | 通过；仅保留既有 dead_code warning |

### 2026-06-28：锦召 `ai-rag-doc` / `ai-rag-service-python` 全 AI 未识别且被算成人工

现场反馈为 `logs/锦召2026-06-28.jsonl` 中 `2026-06-27 21:51:10` 提交的 `ai-rag-doc`、`2026-06-27 21:53:01` 提交的 `ai-rag-service-python` 都是 AI 生成，但没有识别出 AI，最终还被识别成人工。两个目标 commit 分别为 `ed33b0486bef6703211f81e5e187c7603b87f865` 和 `060e994c634216282316fa4be9112b4e9cb8a6c2`，本机版本为 `2.2.47`。这次坏数不是服务端改写，也不是 HTTP 上传问题：本地 `post_commit_stats_computed` 已经先算错，随后 `wrapper_post_commit` 和 `auto` 两路都以 HTTP 200 正常上传了本地坏数。

关键链路：

| 时间 | 事件 | 结论 |
|------|------|------|
| 21:50:55 / 21:52:30 | 两个项目分别 `git add .` | 提交前日志中没有对应项目的 `checkpoint github-copilot` / `checkpointKind=ai_agent` 事件 |
| 21:51:10 | `ai-rag-doc` commit `ed33b048...` | commit 由 2.2.47 拦截 |
| 21:51:11 | `ai-rag-doc post_commit_working_log_loaded`：`checkpointKindCounts={human:1}`、`aiCheckpointCount=0` | 当前 base working log 只有 1 个覆盖 10 个文件的 `Human` checkpoint，没有 prompt / session / AI 证据 |
| 21:51:11 | `ai-rag-doc post_commit_legacy_human_manual_gaps_filled`：`filledLineCount=2995` | 2995 条新增行被写成 commit author 的 `h_*` 人工 |
| 21:53:01 | `ai-rag-service-python` commit `060e994...` | commit 由 2.2.47 拦截 |
| 21:53:03 | `ai-rag-service-python post_commit_working_log_loaded`：`checkpointKindCounts={human:1}`、`aiCheckpointCount=0` | 当前 base working log 只有 1 个覆盖 14 个文件的 `Human` checkpoint，没有 AI 证据 |
| 21:53:03 | `ai-rag-service-python post_commit_legacy_human_manual_gaps_filled`：`filledLineCount=250` | 250 条新增行被写成 commit author 的 `h_*` 人工 |
| 21:51:13-15 / 21:53:05-06 | `upload_stats_succeeded` | 上传链路正常，只是上传了本地已经算错的人工作数 |

根因分两层：

第一层是“为什么 AI 没识别出来”。这两个目标提交之前，对应子仓库没有任何 Copilot AI checkpoint 进入 post-commit：`aiCheckpointCount=0`、`pathspecCount=0`、`promptCount=0`、`sessionCount=0`、`attestationFileCount=0`。按审计语义，post-commit 没有 AI 证据时不能凭用户事后确认直接写 AI。进一步查看同一份日志的后续 `23:19` 场景，Copilot 在 `D:\idea_workspace\ai-rag` 上层工作区触发 `apply_patch`，同一个 payload 同时包含上层非 Git 仓库文件 `D:/idea_workspace/ai-rag/CHANGELOG.md` 和子仓库 `ai-rag-doc` 内多个文件。旧 `build_checkpoint_files(...)` 遇到任一“找不到 Git 仓库”的路径会直接返回错误，导致同批里本来能归到子仓库的 AI 文件也没有发送 `ai_agent` checkpoint。这解释了该类多项目工作区里“AI 工具事件存在，但目标子仓库没有 AI checkpoint”的可复现漏采路径；21:51 / 21:53 两个提交本身在日志里没有相邻 Copilot 进程，因此不能从现有证据补造历史 AI attestation，只能修客户端后续漏采。

第二层是“为什么没有 AI 证据时又被算成人工”。这两个提交的 working log 中唯一的 `Human` checkpoint 是 replay/backfill 快照类证据，本应带 `git_ai_replay_checkpoint=true`，只能表示提交重放 / 快照兜底，不是强人工证据。但 `src/daemon/checkpoint.rs` 旧逻辑只为 AI、AI pre-edit、降级 KnownHuman 和有效 KnownHuman 保存 metadata，普通 `CheckpointKind::Human` 即使请求带 metadata 也会落成 `agent_metadata=None`。post-commit 侧 `is_plain_legacy_human_checkpoint(...)` 因此把它误判成“plain legacy manual Human”，触发 `fill_legacy_human_manual_gaps_for_commit(...)`，最终分别把 2995 行和 250 行写成 `h_*` 人工。

本次代码级修改：

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `src/commands/checkpoint_agent/orchestrator.rs` | `build_checkpoint_files(...)` 对单个无 Git 仓库归属的路径记录 `checkpoint_file_path_skipped(reason=no_git_repository)` 并跳过，不再整批失败 | Copilot 在上层工作区一次 payload 混入 `CHANGELOG.md` 这类非仓库文件时，子仓库内可归属路径仍会生成 AI checkpoint |
| `src/daemon/checkpoint.rs` | 普通 `CheckpointKind::Human` 在请求 metadata 非空时也保留 `agent_metadata` | `git_ai_replay_checkpoint=true`、AI pre-edit close 等弱/诊断标记不会在落盘时丢失 |
| `tests/integration/github_copilot_tools.rs` | 新增 `copilot_mixed_workspace_paths_skip_non_repo_and_keep_nested_repo_ai` | 覆盖 `ai-rag` 这类上层工作区 + 子 Git 仓库混合路径的 Copilot 漏采 |
| `tests/integration/repos/test_repo.rs` | 增加通过 daemon 路径写入带 metadata legacy Human checkpoint 的测试 helper | 回归测试覆盖真实“daemon 入库 -> working log -> post-commit”链路 |
| `tests/integration/post_commit_unit.rs` | 新增 `replay_human_checkpoint_does_not_fill_manual_gaps` | 锁定 replay Human 不再触发 manual gap-fill；无 AI 证据时新增行保持 `unknown`，而不是误报人工 |

版本与历史数据口径：客户端修复保证后续同类 mixed workspace payload 不再因为一个非仓库路径丢掉整批 AI checkpoint，同时 replay Human 不再因 metadata 丢失被强行补成人工。对于已经上传的 `ed33b048...` 和 `060e994...`，远端历史记录不会自动改变；在当前日志没有 AI checkpoint 证据的前提下，重新生成后也应按审计语义保持 `unknown`，不能仅凭事后确认直接改成 AI。只有能提供对应 Copilot hook / authorship note / checkpoint 证据时，才可做历史 AI 归因修复并重传。

验证记录：

| 命令 | 结论 |
|------|------|
| `cargo fmt --check` | 通过 |
| `cargo test copilot_mixed_workspace_paths_skip_non_repo_and_keep_nested_repo_ai --test integration` | 通过 |
| `cargo test replay_human_checkpoint_does_not_fill_manual_gaps --test integration` | 通过 |
| `cargo test test_replace_string_in_file_basic --test integration` | 通过，确认普通 Copilot 文件编辑归因未破坏 |

### 2026-06-22：闭环同分支 pull 后 AI 代码被保存事件归成人工

现场形态：项目 A 的 `test` 分支上，用户1先用 AI 生成了未提交代码；用户2在同一分支手写并 push；用户1 pull 用户2的提交后，IDE/文件监听又触发一次 `KnownHuman` 保存 checkpoint，随后用户1 commit。旧逻辑只看 pull 后新 base 的 active working log，已经看不到用户1 pull 前那份 AI checkpoint；如果保存事件又生成 `h_*` 人工 attestation，最终本地 note / stats 会把用户1的 AI 代码算成人工。这不是服务端看板映射问题，也不是上传字段问题，坏数在 post-commit authorship note 里已经形成。

本次修复把“base/archived working-log 断档”补成 checkpoint 和 post-commit 双保险：

| 文件 | 变更点 | 作用 |
|------|--------|------|
| `src/authorship/archived_ai_state.rs` | 新增归档 `old-*` working log 的 recent AI state 采集，复用 line-attribution 到 char-attribution 的还原逻辑 | 统一 checkpoint 与 post-commit 对旧 AI 证据的读取方式，避免各自散落实现 |
| `src/daemon/checkpoint.rs` | `KnownHuman` 原始请求即触发归档 AI state 合并，不再只看降级后的 `effective_kind` | 弱 `KnownHuman` 被降级成 `Human` 时，也能从旧 base 恢复同文件 AI 历史，避免保存事件整文件抢人工 |
| `src/authorship/post_commit.rs` | 在 note 写入前增加归档 AI 回填：仅处理当前 active working log 没有 AI 证据的新增文件；只有当前提交新增行与归档 AI 快照同文件、同行同内容，或非空文本在两边都唯一匹配时，才把对应行从 `h_*`/unknown 恢复到原 AI author id | 即使 checkpoint 阶段漏过，也不会让已存在的归档 AI 证据被本次保存事件永久盖成人工；同时避免把用户2或用户1真实人工新增误补成 AI |
| `tests/integration/pull_rebase_ff.rs` | 新增 `test_fast_forward_pull_then_known_human_save_preserves_archived_ai_attribution` | 覆盖“用户1 AI 未提交 -> 用户2 push -> 用户1 fast-forward pull -> IDE KnownHuman save -> commit”的回归形态 |

诊断上新增 `post_commit_archived_ai_attributions_restored`，记录 `restoredLineCount`、`restoredFileCount`、`candidateFileCount`、`currentAiFileCount` 和文件样例。若这条日志出现，说明本次 commit 确实通过归档 old-base AI 证据修复了断档；若没有出现，要继续查是否根本没有 AI checkpoint、路径没采到、归档已超过 24 小时窗口，或内容已被人工改写导致不能安全匹配。

本轮没有执行 `cargo test` / 集成测试：当前 Windows 环境每次跑测试会把机器卡死。后续验证建议只跑单条回归 `cargo test test_fast_forward_pull_then_known_human_save_preserves_archived_ai_attribution --test integration`，并先确认没有残留 `cargo` / `rustc` 进程。

## 最近一周归因不准问题总览（2026-06-11 至 2026-06-17）

最近一周看板“统计不准”不是同一个公式错误，而是归因输入、checkpoint 落盘、post-commit gap-fill、上传补算和安装生效几个环节分别暴露问题。排查时必须先判断本地 `post_commit_stats_computed.statsSummary` 是否已经算错：如果本地 `aiAdditions` / `humanAdditions` / `unknownAdditions` 已经异常，根因在归因链路；如果本地正确但看板不对，再查上传 payload、后端响应和逐文件路径匹配。

| 症状 | 最近一周根因 | 典型现场 | 修复/判断口径 |
|------|--------------|----------|---------------|
| AI 生成被识别为人工 | AI 工具写入后触发 IDE / VS Code 保存事件，弱 `KnownHuman` 或 recent `KnownHuman` 抢先写入 `h_*` 人工 attestation；旧逻辑把“保存”当成“人手写完整文件” | 王治尧 `2026-06-12` 全 AI 代码被算出大量 `humanAdditions`；胡晴琨 `op-api` `2026-06-17 16:29:36` 提交中 AI Edited 后紧跟 `known_human` | 弱 `KnownHuman` 缺少有效 `kh_editor` 时降级，不再强归人工；recent AI 后 1 秒内拒绝保存抢占，30 秒内只归因真实新增/改动行 |
| AI 生成完全未识别，落入 `unknown` | Copilot native / CLI hook 的 `tool_input` / `tool_response` 不带路径，真实路径只在顶层 `dirtyFiles` / `dirty_files` 的 key；旧代码只把 `dirtyFiles` 当快照内容，没把 key 当路径兜底 | `rj-ltc-contract-web`、`ai-cr-manage-service`、`op-api`，以及 `rj-ltc-web` 这种“代码全是 AI，最终只识别少量行”的场景 | GitHub Copilot `ide` / `cli` preset 已用 `file_paths_from_dirty_files(...)` 做路径兜底；看 `bash_post_checkpoint_resolved.dirtyFilePathCount` / `mergedFromDirtyFiles` / `finalFileCount` |
| AI 生成部分未识别，文件已经进了 AI checkpoint 但尾段是 `unknown` | 旧 gap-fill 只补“完全没进 pathspec 的漏文件”，没有补“同一个文件已有部分 AI line range，但本次新增尾段没有任何 line-range attestation”的缺口 | 王江龙 `ltc-platform` `2026-06-17 22:00:44`，`CrAiParseFlowServiceImpl.java` 新增 298 行，159 行 AI，139 行 unknown | 新 gap-fill 在已有当前 AI attestation 落地的文件内补未归因新增行，并受 `totalAiAdditions >= 已落地 AI 行 + 待补缺口行` 预算保护；看 `post_commit_ai_attribution_gaps_filled.unattributedAddedLineCount` / `landedAiFileCount` |
| 人工文件被误补成 AI | 旧 AI gap-fill 用“整次 commit 新增行”补 AI 缺口，遇到同一提交里同时有 AI 文件和人工文件时，可能跨文件把没有 AI 证据的新增行也补到当前 AI attestation | `git-ai` 最近提交中 `Cargo.toml` / `Cargo.lock` 本应为人工或未归因，却被看板显示为 AI | AI gap-fill 只能处理有 AI checkpoint 路径证据或已有当前 AI attestation 落地的文件；主指标按最终落地 attestation 归因，`mixedAdditions` 不再并入 `aiAdditions`；日志新增 `includedCandidateFileSample` / `excludedCandidateFileSample` / `pathspecSample` |
| AI 生成部分未识别，bash / terminal 场景漏文件 | Copilot terminal / bash post 在 `MissingPreSnapshot`、`SnapshotFailed`、`HookTimeout` 或 post-hook error 时旧逻辑直接放弃 post checkpoint；或者只用窄 stat diff，没合并 `dirtyFiles` 里的真实文件路径 | 张一峰 `op-api` `2026-06-11 13:58:10`、`rj-ltc-web` Copilot bash dirty files 漏归因 | `PostBashCall` 已携带 `dirty_files`，post 阶段合并 dirty file paths，并在异常时回退 `git_status_fallback`；看 `bash_post_checkpoint_resolved.fallbackReason`、`detectedPathCount`、`dirtyFilePathCount`、`finalFileCount` |
| 路径已发现，但 checkpoint 文件被 daemon 过滤，最终 `unknown` | 既有文本文件含非 UTF-8 字节，旧 checkpoint 用 `read_to_string(...).ok()` 读取失败后 `content=None`，daemon 又只保留带内容的文件 | `op-return-exchange` 多个 `.java` / `.properties` 文件，`bash_post_checkpoint_resolved` 已发现 90 个文件，但 daemon 只解析 79 个 | checkpoint 改为字节读取并用 `String::from_utf8_lossy` 生成文本快照，daemon 对 `content=None` 增加 workdir 兜底读取 |
| 人工新增被识别不出来，落入 `unknown` | legacy `Human` checkpoint 的语义历史上偏“基线/未知”，明确人工新增没有稳定生成 `h_*` attestation | 张彪 `contract` `2026-06-16` 多次人工提交被归为 `unknown` | 已区分 explicit manual 与 AI pre-edit baseline；排查时看 `checkpoint_attribution_decision.kind=human/known_human` 是否真的写入人工 attestation |
| 空白行或中文路径导致逐文件看起来不准 | whitespace-only 新增行不在 note line range 内会被直接算 unknown；`core.quotepath=true` 会把中文路径转义，导致逐文件 numstat 与 note 路径对不上 | 方法尾部空白行差 1 行；中文文件名逐文件 `aiAdditions` 掉到 `unknownAdditions` | stats / upload 已对相邻空白新增行做归并；逐文件 numstat 显式使用 `git -c core.quotepath=false diff-tree --numstat` |
| 上传/看板看起来不准，但归因本体没错 | post-commit 没运行、stats 被大提交保护跳过后上传没补算、自动更新先重启导致本次 commit 未上传、HTTP 200 但后端业务 `code != 200` 被误当成功 | 坏 `core.hooksPath` 指向旧目录、`post_commit_stats_skipped(reason=expensive_commit)` 后 `upload_stats_skipped(reason=stats_unavailable)`、安装/更新期间 daemon 新旧混跑 | 先确认 `git_proxy_entered` / `post_commit_*` 是否存在；大提交上传已支持后台补算；commit 成功后先 fallback upload 再调度自更新；上传成功必须同时校验 HTTP 和响应体 `code=200` |
| 模型被统一显示成 `gpt-5.3-codex` | Copilot VS Code native transcript 没有写实际请求模型时，旧逻辑回退读取 `debug-logs/<session>/models.json` 的聊天默认模型；默认模型不是本次请求实际模型 | `C:\Users\admin\.git-ai\logs` 中 `github-copilot` 记录出现 `copilot/gpt-5.3-codex`，但当前 `git-ai` HEAD note 实际是 `codex::gpt-5.5` | 归因链路不再使用 `models.json` 默认模型兜底；transcript 有实际模型才写入，否则写 `unknown`，并记录 `checkpoint_copilot_model_resolved.modelSource` 与 `checkpoint_copilot_models_json_default_ignored` |

当前最容易混淆的是三件事：

1. `unknownAdditions` 不是人工行，只表示“本次 commit 新增行没有被任何 AI 或人工 attestation 覆盖”。没有 AI checkpoint 时，即使用户事后确认是 AI，也不能直接把历史 unknown 硬改成 AI，否则会破坏审计语义。
2. “AI 被识别成人工”和“AI 没识别出来”要分开看。前者通常能在 `checkpoint_attribution_decision.kind=known_human`、`decision=human`、`h_*` attestation 中看到证据；后者通常表现为 `aiCheckpointCount=0`、路径为空、daemon 解析文件数少于 hook 检测文件数，或同文件存在未覆盖的新增 line range。
3. “本地统计不准”和“远程看板不准”也要分开看。若 `post_commit_stats_computed.statsSummary` 已经异常，优先查 checkpoint / authorship note；若本地 stats 正确但远程异常，优先查 `upload_stats_payload_build_succeeded`、`upload_stats_response_*`、逐文件 path key、`hasAuthorshipNote` 和后端业务响应。

现场日志建议按下面顺序看：

| 要判断什么 | 优先看哪些事件 |
|------------|----------------|
| 本次提交有没有归因输入 | `post_commit_working_log_loaded.checkpointSummary`，尤其是 `aiCheckpointCount`、`promptCount`、`sessionCount`、`attestationFileCount` |
| Copilot / bash 是否拿到了文件路径 | `bash_post_checkpoint_resolved` 的 `detectedPathCount`、`dirtyFilePathCount`、`mergedFromDirtyFiles`、`fallbackReason`、`finalFileCount` |
| checkpoint 是否被写入或被过滤 | `checkpoint_request_send`、`checkpoint_request_started`、`checkpoint_request_finished`、`daemon_checkpoint_request_resolved_files`、`checkpoint_no_entries` |
| AI 是否被 IDE 保存抢成人工 | `checkpoint_attribution_decision`、`known_human_checkpoint_rejected`、`diagnosticHints`、`previousCheckpoint.kind`、`fileDiagnostics[].reason` |
| AI 文件内部是否还有未覆盖尾段 | `post_commit_ai_attribution_gaps_filled` 的 `missingAddedLineCount`、`unattributedAddedLineCount`、`landedAiFileCount` |
| 本地最终统计是否已经异常 | `post_commit_stats_computed.statsSummary.gitDiffAddedLines`、`aiAdditions`、`humanAdditions`、`unknownAdditions` |
| 是不是上传/看板链路异常 | `post_commit_stats_skipped`、`upload_stats_skipped`、`upload_stats_ready`、`upload_stats_payload_build_succeeded`、`upload_stats_response_*`、`hasAuthorshipNote` |

### 2026-06-19：6 月 16-18 日现场问题统一复盘与根因边界

这几天的问题不能继续按“AI / 人工 / unknown 算错”一个大类处理。所有可核对现场都有一个共同点：坏数已经出现在本地 `post_commit_stats_computed` 或 `upload_stats_payload_build_succeeded.payloadSummary.stats` 中，上传只是把本地 note / stats 结果送到看板；因此第一根因不是后端看板，也不是 HTTP 上传字段映射。

统一时间线如下，统计口径为 `gitDiffAddedLines / aiAdditions / humanAdditions / unknownAdditions`：

| 时间 | 用户与项目 | 版本 / commit | 本地统计证据 | 根因定位 |
|------|------------|---------------|--------------|----------|
| 2026-06-16 14:09 | 张艺锋 `ecp-rule` | `2.2.37` / `891bc812` | `3 / 1 / 0 / 2`，working log 有 `ai_agent,human,known_human`，但 `attestationFileCount=1`、`promptCount=1` | 同一文件 `OpportunityAssert.java` 有 AI checkpoint，但最终只有 1 行 AI line-range 落地，剩余 2 行没有 AI 或 `h_*` 覆盖。属于“文件已进入 AI pathspec，但新增行内部还有未覆盖尾段/缺口”的小样本，不是看板错。 |
| 2026-06-16 14:43 | 庞福泽 `rj-ltc-contract-web` | 相邻进程 `2.2.37` / `6925d059` | `3519 / 790 / 174 / 2557`，`promptCount=33`、`pathspecCount=14`、`attestationFileCount=13` | AI 会话数量和总 AI 预算足够，但真正 materialize 出来的文件只有 13 个，大量 AI 生成文件没有路径/line attestation；另有 `KnownHuman` 保存把部分 AI 行抢成人工。根因是 Copilot payload 缺路径时旧逻辑没有用 `dirtyFiles` key 兜底，加上 recent `KnownHuman` 防护不足。 |
| 2026-06-16 15:17 | 张彪 `contract` | `2.2.37` / `f8788d0`、`5b250d3`、`a4a1279` | 分别为 `2 / 0 / 0 / 2`、`1 / 0 / 0 / 1`、`2 / 0 / 0 / 2`；其中 `f8788d0` 有 `checkpointKinds=human` 但 `attestationFileCount=0` | 人工提交没有生成可计入 `humanAdditions` 的 `h_*`。旧 `Human` checkpoint 只是 baseline/sentinel，统计链路会跳过 `"human"` sentinel，所以人工新增落到 unknown。现有日志未找到用户提到的 14:59/15:02 对应 2026-06-16 post-commit 记录，但 15:17 三条能证明同一根因。 |
| 2026-06-16 16:31 | 徐建丰 `ai-cr-manage-service` | `2.2.37` / `e2d2966` | `248 / 0 / 5 / 243`，working log 只有 `human,known_human`，`aiCheckpointCount=0`、`promptCount=0` | 这次 commit 的 parent/base working log 里已经没有 AI checkpoint，只有一个覆盖 9 文件的 `human` baseline 和一个 `known_human` 文件。早些时候同项目有 AI checkpoint，但没有被当前 base 继承；旧版本没有 baseCommit/old-base 诊断和归档 AI state 合并，所以多数 AI 证据在本次归因视角下不可见。 |
| 2026-06-17 16:29 | 胡晴琨 `op-api` | `2.2.39` / `23a81b7` | `63 / 20 / 43 / 0`，16:24 同一 `toolUseId` 先 `human WillEdit`、再 `ai_agent Edited`、随后 `known_human Edited` | 不是旧版本 2.2.37 问题，也不是上传错。AI 写完后 IDE 保存/文件监听触发 `KnownHuman`，旧逻辑把保存事件当成人工编辑，43 行被写成 `h_*`。1 秒拒绝窗口挡不住 1-30 秒内的保存竞态，必须让 recent `KnownHuman` 只认领真实新增/改动行。 |
| 2026-06-17 20:47 | 龙宇 `rj-ltc-web` | `2.2.40` / `1c9c98ed` | `75 / 30 / 45 / 3`，working log 有 `ai_agent,ai_tab,human,known_human`，`aiCheckpointCount=20` | 用户确认全 AI，但本地已经把 45 行算成人工。根因仍是 AI checkpoint 后的 `KnownHuman` / mixed 归因抢占；不能在上传层修，只能在 checkpoint/state 层避免保存事件覆盖已有 AI 行，并把 `mixedAdditions` 从 AI 主计数中剥离。 |
| 2026-06-17 22:00 | 王江龙 `ltc-platform` | `819ae3f` | commit 级 `592 / 453 / 0 / 139`；目标文件新增 298 行，159 行 AI、139 行 unknown | AI checkpoint 已经命中 `CrAiParseFlowServiceImpl.java`，但旧 gap-fill 只补完全漏 pathspec 的文件，不补“同一 AI 文件内部未被 line-range 覆盖的新增尾段”。 |
| 2026-06-18 15:26 | 王浩亮 `united-learning-association` | `2.2.41` / `fb6a8e3` | `9 / 0 / 1 / 8`，working log 只有 `human,known_human`，`aiCheckpointCount=0`；后续 parent backfill 多个 commit 也 `checkpointCount=0`、全 unknown | 目标 `RoleList.vue` 没有任何 AI checkpoint / prompt / path evidence。若实际为 AI，丢证据发生在 Copilot hook 路径采集或 base working log 继承之前；post-commit 只能保持 unknown，不能事后硬判 AI。 |
| 2026-06-18 16:05 | 王治尧 `rj-ltc-contract-web` | `2.2.41` / `87ded2d` | `1114 / 1085 / 19 / 10`，另有 `mixedAdditions=10` | 目标 `ProjectTracking/index.vue` 只出现在 `human/known_human` 或宽 dirty 文件集合中，没有 AI Edited 路径证据；如果把所有 dirtyFiles 无条件并入 AI，会重新制造 `Cargo.toml` / `Cargo.lock` 被误判为 AI 的问题。修复方向是保留 `WillEdit` 的 `tool_use_id`，仅当后续同工具调用真的有 AI Edited 时才作为 AI gap-fill 候选。 |
| 2026-06-18 17:55 | 李小杰 `rj-ltc-contract-web` | `2.2.41` / `41bf356` | `587 / 0 / 582 / 5` | 早些时候确有 AI checkpoint，但 `41bf356` 当前 parent/base 的 active working log 只剩 `human,known_human`，AI 证据在更早 base / `old-*` 归档里；`KnownHuman` / replay human 在看不到 AI 历史时把文件现态写成强人工。根因是 base/archived working-log 断档，不是 AI checkpoint 从未出现。 |
| 2026-06-18 18:13 | 王江龙 `ltc-platform` | `4765dd1` | `181 / 147 / 18 / 16`，另有 `mixedAdditions=3`；三份 `application-*.properties` 人工新增落 unknown | Copilot `PreToolUse` 对 properties 写入 `Human + WillEdit`，后续 `PostToolUse no_changes` 没有关闭同 `tool_use_id` 的 AI pre-edit 基线；VS Code 扩展/daemon 队列又可能吞掉后续真实 `KnownHuman` 保存，最终只剩弱 human baseline，没有 `h_*`。 |

#### 现场问题明细表（按日志定位）

> 下面这张表把“哪个人、哪个时间、哪个日志、什么问题、根因、解决方案”落到可复查字段。`解决方案 / 状态` 只表示客户端侧已经补齐的防复发路径；已经上传到看板的历史 commit 不会因用户升级自动改写，若要修历史看板，必须另走 authorship note 重算和重传流程。

| 用户 / 现场 | 提交时间 | 日志文件 | 项目 / 文件 | commit / 版本 | 现场问题 | 问题根因 | 解决方案 / 状态 |
|---|---|---|---|---|---|---|---|
| 张艺锋 | 2026-06-16 14:09:01 | `logs/张艺锋.jsonl` | `ecp-rule` / `OpportunityAssert.java` | `891bc812` / `2.2.37` | 用户总共新增 3 行，实际只识别 1 行 AI，剩余 2 行既不是 AI 也不是人工，落到 `unknown`。 | 日志有 AI checkpoint、`promptCount=1`、`attestationFileCount=1`，说明 AI 输入不是完全缺失；真正问题是 AI line-range 只落地 1 行，同文件内部还有新增尾段没有任何 AI 或 `h_*` 覆盖。 | 已用“AI 文件内部 gap-fill”补同文件未归因新增行，并增加 `post_commit_ai_attribution_gaps_filled`、`unattributedAddedLineCount`、`landedAiFileCount` 诊断。历史 `891bc812` 需重算重传才会改看板。 |
| 庞福泽 | 2026-06-16 14:43:34 | `logs/庞福泽.jsonl` | `rj-ltc-contract-web` | `6925d059` / 相邻进程 `2.2.37` | 用户确认代码基本全部 AI 生成，但本地只算出 `790` 行 AI、`174` 行人工、`2557` 行 unknown。 | `promptCount=33`、`aiCheckpointCount=20`，但 `pathspecCount=14`、`attestationFileCount=13`，大量 AI 生成文件没有被 materialize 成 line attestation；同时 IDE 保存类 `KnownHuman` 抢占了部分 AI 行。根因是 Copilot payload 缺路径时未用 `dirtyFiles` key 兜底，加上 recent `KnownHuman` 防护不足。 | Copilot preset 已加 `dirtyFiles/dirty_files` 路径兜底，且只在 payload 没有明确路径时使用；recent/weak `KnownHuman` 只能认领真实 changed lines。历史提交不自动改。 |
| 张彪 | 2026-06-16 14:59:24、15:02:23、15:17:43 | `logs/张彪.jsonl` | `contract` | `f8788d0`、`5b250d3`、`a4a1279` / `2.2.37` | 代码实际人工编写，但多次提交被算成 unknown；15:17 附近可复现为 `2 / 0 / 0 / 2`、`1 / 0 / 0 / 1`、`2 / 0 / 0 / 2`。 | 旧版 `Human` checkpoint 只代表 baseline/sentinel，不会稳定生成 `h_*` 人工 attestation；统计层又会剥离 `"human"` sentinel，所以明确人工新增也落到 unknown。2.2.38 仍没有这条 legacy Human 强人工修复，不能判定升级到 2.2.38 已解决。 | legacy/plain `Human` checkpoint 在满足“非 AI pre-edit、非弱 KnownHuman、非 replay/backfill 脏证据”时，可为实际新增/改写行生成 commit-author `h_*`。历史数据需重算。 |
| 徐建丰 | 2026-06-16 16:31:25 | `logs/徐建丰.jsonl` | `ai-cr-manage-service` | `e2d2966` / `2.2.37` | 用户说明大多数代码 AI 写、人工调整一部分，但本地 `248` 行新增里只有 `5` 行人工，`243` 行 unknown，AI 为 0。 | 目标 commit 的 parent/base working log 只有 `human,known_human`，`aiCheckpointCount=0`、`promptCount=0`；早些时候同项目出现过 AI checkpoint，但没有被当前 base 继承，旧版本缺 base/old-base 诊断和归档 AI state 合并。 | 已补 `baseCommit`、`archiveBase` 诊断，并在同文件存在真实 AI Edited 证据时从 `old-*` 归档合并 recent AI state；没有证据的历史行仍保持 unknown，不能硬判 AI。 |
| 胡晴琨 | 2026-06-17 16:29:36 | `logs/胡晴琨.jsonl` | `op-api` / `feature-20260601-ebg-ecp-h` | `23a81b7` / `2.2.39` | 代码应全部 AI 生成，但 `63` 行新增里 `43` 行被判断成人工。 | 同一 `toolUseId` 先出现 `human WillEdit`、再 `ai_agent Edited`、随后 `known_human Edited`；旧逻辑把 AI 写完后的 IDE 保存/文件监听事件当成人工编辑，写成 `h_*`。这不是上传错，也不是 2.2.37 老版本问题。 | recent/weak `KnownHuman` 已改为只认领真实新增/改动行，不能重领已有 AI 行；补充保存竞态回归测试。历史看板需重算重传。 |
| 龙宇 | 2026-06-17 20:47:15 | `logs/龙宇.jsonl` | `rj-ltc-web` | `1c9c98ed` / `2.2.40` | 用户确认项目代码全是 AI 生成，但当前只识别出 30 行 AI，45 行被算成人工，另有 3 行 unknown。 | working log 有 `ai_agent,ai_tab,human,known_human` 且 `aiCheckpointCount=20`，说明 AI 证据存在；误差来自 AI checkpoint 后的 `KnownHuman` / mixed 归因抢占，旧统计还存在把 mixed 并入 AI 主口径或混淆最终责任人的风险。 | `KnownHuman` 保存事件不再覆盖已有 AI 行；主口径以最终落地 attestation 为准，`mixedAdditions` 单独展示，不并入 `aiAdditions`。历史记录不自动改。 |
| 王江龙 | 2026-06-17 22:00:44 | `logs/王江龙-20260617.jsonl` | `ltc-platform` / `src/main/java/com/ruijie/ltc/service/impl/CrAiParseFlowServiceImpl.java` | `819ae3f` | 目标文件新增 298 行，AI 只识别 159 行，另有 139 行 unknown；commit 级为 `592 / 453 / 0 / 139`。 | AI checkpoint 已命中目标 Java 文件，但旧 gap-fill 只补“完全漏 pathspec 的文件”，没有补“同一个 AI 文件内部已有部分 AI line-range、尾段未覆盖”的情况。 | 已加 AI 文件内部 gap-fill，并用 AI 预算保护防止跨文件误补。新增诊断可以区分“已落地 AI 文件内部缺口”和“完全没有 AI path evidence”。 |
| 本机 `git-ai` 最近提交 | 2026-06-18 至 2026-06-19 排查 | `C:\Users\admin\.git-ai\logs` | `git-ai` / `Cargo.toml`、`Cargo.lock`、`src/commands/checkpoint_agent/presets/*.rs` | 最近一次本地提交 | `Cargo.toml`、`Cargo.lock` 实际人工写，却被识别为 AI；部分 `presets/*.rs` 文件又识别不出人工或 AI；模型还被显示成 `gpt-5.3-codex`。 | 一条根因是 AI gap-fill 过宽，按整次 commit 新增行补 AI 缺口会把同一提交里的人工文件也补成 AI；另一条根因是 Copilot transcript 没实际模型时旧逻辑从 `models.json` 读取聊天默认模型，默认模型不等于本次请求模型。 | AI gap-fill 只能处理有 AI Edited 路径证据、同 `tool_use_id` 的有效前置 WillEdit、或已有 AI attestation 文件内部缺口；`dirtyFiles` 只做兜底，不做无条件 AI 证据。模型字段不再用 `models.json` 默认值兜底，拿不到实际模型时写 `unknown` 并记录 model source 诊断。 |
| 王浩亮 | 2026-06-18 15:26:53 | `logs/王浩亮.jsonl` | `united-learning-association` / `src/views/systemSettings/RolePermissionSection/RoleList.vue` | `fb6a8e3` / `2.2.41` | `RoleList.vue` 未识别为 AI；本地为 `9 / 0 / 1 / 8`，8 行 unknown。 | 上传成功且 payload 已是坏数；working log 只有 `human,known_human`，`aiCheckpointCount=0`，没有任何 `checkpoint github-copilot`、`ai_agent` 或 Copilot tool path 证据。若实际为 AI，证据丢在采集入口或 base 继承之前。 | 不能把历史 8 行 unknown 硬改 AI；已补 `post_commit_attribution_gap_detected` 对 partial unknown 的诊断，后续可直接看 checkpoint summary、prompt summary 和 pathspec。 |
| 王治尧 | 2026-06-18 16:05:33 | `logs/王治尧-0618.jsonl` | `rj-ltc-contract-web` / `src/views/PmpManagement/ProjectTracking/index.vue` | `87ded2d` / `2.2.41` | `ProjectTracking/index.vue` 的代码归因没有识别出来；commit 级为 `1114 / 1085 / 19 / 10`，另有 `mixedAdditions=10`。 | 目标文件只出现在 `human/known_human` 或宽 dirty 集合里，没有 AI Edited 路径证据；如果把所有 dirtyFiles 无条件并入 AI，会复现 `Cargo.toml` / `Cargo.lock` 误判 AI。 | Copilot 路径解析改为“payload 明确路径优先，payload 无路径才用 dirtyFiles key 兜底”；前置 `WillEdit` 只有同 `tool_use_id` 后续确有非 bash AI Edited 时才作为 AI gap-fill 候选。历史 `87ded2d` 不自动改。 |
| 李小杰 | 2026-06-18 17:55:44 | `logs/李小杰-20260618.jsonl` | `rj-ltc-contract-web` | `41bf35693d16df5fce0842d97f0108e8b16f3341` / `2.2.41` | 用户确认代码应全是 AI 生成，但本地已经算成 `587 / 0 / 582 / 5`，几乎全是人工。 | 同一天早些时候相关文件确有 AI checkpoint，但目标 commit 的 parent/base active working log 只剩 `human,known_human`，`aiCheckpointCount=0`；AI 证据被移到更早 base 或 `old-*` 归档里，当前视角看不到，随后 `KnownHuman` / replay human 把文件现态当成强人工。upgrade 只是时间相邻，不是清掉 checkpoint 的直接原因。 | daemon 已支持在当前 base 缺 AI 历史时，从 24 小时内 `old-*` 归档合并同文件真实 AI Edited state；post-commit 拒绝 replay human 冒充强人工，并补 `baseCommit/archiveBase` 诊断。历史 `41bf356` 需重算重传。 |
| 王江龙 | 2026-06-18 18:13:50 | `logs/王江龙-20260618.jsonl` | `ltc-platform` / `src/main/resources/application-dev.properties`、`application-test.properties`、`application-uat.properties` | `4765dd13959e8f626c04f8e84795ab97496970ac` / `2.2.41` | 三个 properties 文件实际人工编写，但既没有识别为 AI，也没有识别为人工，落到 unknown；commit 级为 `181 / 147 / 18 / 16`。 | Copilot `PreToolUse` 对 properties 写入 `Human + WillEdit`，旧逻辑把前置状态当成未关闭 AI pre-edit；对应 `PostToolUse no_changes` 没有关闭它，VS Code/daemon 又可能吞掉后续真实 `KnownHuman` 保存，最终只剩弱 human baseline，没有 `h_*`。另一个根因是 orchestrator 曾对所有 `PreFileEdit` 无条件写 `ai_pre_edit=true`，连 `agent-v1 type=human` 也会被当成 AI pre-edit。 | `PostToolUse no_changes` 会发送 `ai_pre_edit.close` 并写 close marker；KnownHuman 改为逐文件降级；orchestrator 只对非 `human/known_human` 的 AI 工具上下文写 `ai_pre_edit`，daemon/post-commit 防御性拒绝人工 tool 的脏 AI pre-edit 元数据。历史 `4765dd1` 不自动改。 |

从这些现场归纳出的根因边界如下：

1. **证据没有采到**：Copilot `tool_input/tool_response` 不带路径、`dirtyFiles` key 未兜底，或 VS Code / GitHub Copilot hooks 没实际触发；表现为 `aiCheckpointCount=0`、`promptCount=0`、`pathspecCount=0` 或目标文件只在 human/known_human 里。徐建丰、王浩亮属于这个边界；历史 commit 只能保持 unknown，修复应落在 hook 采集、自愈安装、base 诊断和 future checkpoint。
2. **证据采到了但落地不完整**：有 AI checkpoint / prompt，但 materialize 的 `attestationFileCount` 或 line range 不覆盖真实新增行；表现为 `promptCount` 明显大于 `attestationFileCount`、同文件只覆盖前半段。张艺锋、庞福泽、王江龙 6 月 17 日属于这个边界，修复应落在 `post_commit` AI gap-fill 和 pathspec/line-range 补偿。
3. **AI 被保存事件抢成人工**：`ai_agent Edited` 后紧跟 `KnownHuman Edited`，旧逻辑把 IDE 保存当成人手写；胡晴琨、龙宇、李小杰的一部分人工作数都属于这个边界。修复必须让 weak/recent `KnownHuman` 降级或只认领真实 changed lines，不能让保存事件重领已有 AI 行。
4. **人工证据不是强人工证据**：legacy `Human` checkpoint 不是 `h_*`，旧统计会剥离 `"human"` sentinel；张彪和王江龙 properties 的 unknown 都落在这个边界。只有明确人工 checkpoint / 有效 `KnownHuman` 才能写 `h_*`，AI pre-edit baseline 和 replay/backfill human 不能冒充强人工。
5. **base working log 断档**：checkpoint 写在早些 base 下，目标 commit 的 parent/base active log 没继承到；李小杰和徐建丰暴露了这个问题。修复要记录 `baseCommit` / `archiveBase`，并只在归档里有同文件真实 AI Edited 证据时合并 recent AI state。
6. **过宽补偿会制造反向误判**：不能因为用户说“全 AI”就把 commit dirty 集合全部补成 AI；`Cargo.toml` / `Cargo.lock` 被误判 AI 就是这个方向的反例。AI gap-fill 只能使用当前 AI Edited 路径、同 `tool_use_id` 前置 WillEdit 路径、或已落地 AI attestation 文件内部未覆盖新增行；`mixedAdditions` 只能单独展示，不能并入 AI 主计数。

版本边界也必须写清楚：`2.2.37` 覆盖了大部分 6 月 16 日现场，缺 dirtyFiles 路径兜底、legacy Human `h_*`、recent KnownHuman 30 秒保护、base/archived AI state 合并等关键能力。检查本地 tag `v2.2.38` 时仍能看到 `virtual_attribution` 跳过 legacy `"human"` sentinel，且没有后续 `file_paths_from_dirty_files(...)` / recent `KnownHuman` changed-lines / archived AI merge 这些修复；因此不能判断“升级到 2.2.38 就修复张彪问题”。`2.2.39` 到 `2.2.41` 虽然已有部分防护，但从胡晴琨、龙宇、王治尧、王浩亮、李小杰现场看，仍缺后续补丁。历史已经上传的坏记录不会因为客户端升级自动改变，需要单独走 authorship note 重算和重传流程。

> **实施补充（2026-06-18，李小杰 `rj-ltc-contract-web` 17:55:44 提交全 AI 代码被算成人工的根因修复）**：针对 `logs/李小杰-20260618.jsonl` 中 `2026-06-18 17:55:44` 提交 `41bf35693d16df5fce0842d97f0108e8b16f3341`，确认问题不在远端看板或上传接口。现场 `upload_stats_http_response_received.statusCode=200`、`upload_stats_http_body_checked.backendCode=200`、`upload_stats_succeeded` 都成功；但本地 `post_commit_stats_computed` 已经是错误结果：`gitDiffAddedLines=587`、`aiAdditions=0`、`humanAdditions=582`、`unknownAdditions=5`。同一天早些时候这些文件确实出现过 `ai_agent Edited` checkpoint，例如 `DeliverySubProjectList/index.vue`、`SubProjectDetailDrawer.vue`、`public/data/deliverySubProjectList.json`；但 `41bf356` 的 parent 是 `87ded2dd5ac6a429b05f96c62778301e4a2574ff`，commit-time 读取的是 `87ded2d` 这份 base working log，而这份 working log 只剩 `human=1`、`known_human=1`，`aiCheckpointCount=0`。所以准确说不是 AI checkpoint 从未写入，而是它写在更早的 base working log / `old-<base>` 归档里，没有被当前 parent/base 继承到这次提交归因。

> 现场链路是：working log 按 base commit 分桶；post-commit 后当前 base 的 working log 会移动为 `old-<base>`。AI 编辑发生在早些时候的 base 下，后续 base 变化、提交回放或 backfill 让 `41bf356` 当前 parent `87ded2d` 的 active working log 只保留了 17:54 左右的 `known_human` 保存事件。于是 `KnownHuman` / replay human 在“当前 base 看不到 AI 历史”的情况下，把文件现态当成强人工或弱人工基线，最终本地 note 已经算成 `humanAdditions=582`。17:58 的 `push_authorship_backfill` 又处理了 parent `87ded2d`，其 working log 为空，结果 `gitDiffAddedLines=1114` 全部 `unknownAdditions=1114`；这说明 parent 本身也没有在 child 提交前 materialize 出可继承的 AI note。日志里的 `upgrade --background` 只是时间上相邻，触发 parent backfill 的路径是 push hook，不是升级命令直接把 checkpoint 清掉。

> 本轮行为修复不是只补日志，落在四处。第一，`src/daemon/checkpoint.rs` 在处理 `KnownHuman` 时会扫描 24 小时内的 `old-*` archived working logs：如果当前 base 对目标文件没有 AI 历史，但归档里有近期非 bash AI Edited checkpoint，就把该文件的 AI state 合并回 previous state，使 IDE 保存事件只能认领真正 changed lines，不能把已有 AI 行整文件抢成人工。第二，恢复归档 state 时必须从归档 working log 自己的 blob 目录读取内容；如果 checkpoint 为节省空间只保留 `line_attributions` 而没有 char-level `attributions`，会用归档 blob 还原 char attribution，避免“状态带回来了但内容读不到”继续漏算。第三，`src/authorship/post_commit.rs` 不再把带 `git_ai_replay_checkpoint=true` 的 replay human 视为强 legacy manual human，避免 backfill/replay 在缺 AI 证据时把行补成人工。第四，`src/authorship/post_commit.rs` 的 `post_commit_working_log_deleted` 日志补出 `archiveBase=old-<parent>` 和归档保留时间；`src/daemon.rs` 的 checkpoint request 日志补出 `baseCommit`，后续能直接看出 checkpoint 写在哪个 base 下面。

> 仍然保留审计边界：有 AI Edited / 同 tool_use_id AI 证据的文件，允许补成 AI；有明确 `KnownHuman` 编辑器元数据或纯 legacy 人工 checkpoint 的新增行，才允许补成人工；只有 AI pre-edit、弱保存事件或找不到落地 AI 证据的行，只能 unknown。本轮新增/更新回归包括 `archived_ai_state_merge_adds_ai_history_for_missing_base`、`archived_ai_state_merge_does_not_replace_current_base_ai_history`、`archived_ai_state_reconstructs_from_line_attributions_and_source_blob`、`plain_legacy_human_checkpoint_rejects_ai_or_downgraded_metadata`、`unclosed_ai_pre_edit_blocks_known_human_attestation`、`test_known_human_after_unclosed_ai_pre_edit_does_not_claim_ai_lines_as_human`、`test_ai_post_edit_after_unclosed_known_human_still_claims_ai_lines`。历史已上传的 `41bf356` 不会被客户端修复自动改写；如果要修看板历史，需要另走明确的历史 note 重算/重传流程。

> **实施补充（2026-06-18，王治尧 `rj-ltc-contract-web` 16:05:33 提交中 `ProjectTracking/index.vue` 未归 AI 的根因与防复发修复）**：针对 `logs/王治尧-0618.jsonl` 中 `2026-06-18 16:05:33` 提交 `87ded2dd5ac6a429b05f96c62778301e4a2574ff`，本轮确认问题不在远端上传或看板入库。现场 `upload_stats_http_response_received.statusCode=200`、`upload_stats_http_body_checked.backendCode=200`、`upload_stats_succeeded` 均已出现；本地 `post_commit_stats_computed` 已经给出 `gitDiffAddedLines=1114`、`aiAccepted=1085`、`aiAdditions=1085`、`humanAdditions=19`、`unknownAdditions=10`、`mixedAdditions=10`。真正根因是本次 working log 中 AI checkpoint 的路径证据只覆盖 4 个 `src/views/PmpManagement/BaselinePool/NewPool/...` 文件，`src/views/PmpManagement/ProjectTracking/index.vue` 只出现在 `human` / `known_human` 路径样本里，没有任何 Copilot AI Edited 路径证据；因此 `post_commit_ai_attribution_gaps_filled` 只能把 4 个 `NewPool` 文件纳入 `pathspecSample`，并明确把 `ProjectTracking/index.vue` 和 `views.js` 放进 `excludedCandidateFileSample`。按审计语义，不能在没有 AI checkpoint / tool path / attestation 证据的历史提交里，把 `ProjectTracking/index.vue` 事后硬改成 AI。

> 本轮代码修复的目标是把这个“缺证据为什么缺”补到采集链路和日志里，并在后续 Copilot file-edit 场景里尽量保住可验证的 AI 前置路径证据。GitHub Copilot `ide` / `cli` preset 现在统一通过 `resolve_filepaths_from_hook_payload_or_dirty_files(...)` 解析路径：优先使用 `tool_input` / `tool_response` 里的明确路径，只有 payload 完全没有路径时才用 `dirtyFiles` / `dirty_files` 的 key 做兜底，并记录 `checkpoint_copilot_paths_resolved`，字段包括 `pathSource`、`payloadPathCount`、`dirtyFilePathCount`、`finalPathCount` 和路径样本。这里不能把所有 `dirtyFiles` 都并入 AI 证据，因为本机日志已经出现过 27/723 个 dirty 快照的宽集合；无条件合并会重新制造 `Cargo.toml` / `Cargo.lock` 被误判为 AI 的假阳性。

> 另一个防复发点是 Copilot `PreToolUse` 的 `WillEdit` 路径。旧版本只把 AI Edited checkpoint 持久化 `agent_metadata`，而 Copilot 前置编辑请求通常落成 `CheckpointKind::Human + pathRole=WillEdit`，导致它的 `tool_use_id` / `edit_kind` 丢失，后续如果 `PostToolUse` 缺路径，就很难证明“这个前置路径和后置 AI 编辑属于同一次工具调用”。当前实现不会把这类前置 checkpoint 改成 AI，也不会设置 `agent_id` 来冒充 AI 生成；只有 agent tool 是真实 AI 工具上下文时，才在 `agent_metadata` 中保留 `tool_use_id`、`edit_kind=file_edit`、`ai_pre_edit=true`、`agent_tool`。`agent-v1 type=human`、`human`、`known_human` 这类人工上下文即使进入 `WillEdit`，也不能写入或继续被识别为 AI pre-edit。post-commit gap-fill 只有在同一个 `tool_use_id` 后续确实出现非 bash 的 AI Edited checkpoint 时，才把这个前置 `WillEdit` 路径作为 AI gap-fill 候选；bash / terminal 场景仍然排除。这样既能修复未来“PostToolUse 漏路径但 PreToolUse 有准确文件路径”的场景，又不会把普通人工保存、宽 dirtyFiles 快照或 terminal 命令误补成 AI。

> 诊断侧，`post_commit_ai_attribution_gaps_filled` / skip 事件新增 `candidateDiagnostics`，逐候选文件输出 `status`、`checkpointKindCounts`、`hasAiPathEvidence`、`hasLandedAiLines`、`aiPreEditPathEvidenceCount`、`agentToolSample`、`toolUseIdSample`。后续再遇到“某个文件没识别出来”，可以直接判断它是 `excluded_no_ai_path_evidence`、`excluded_no_checkpoint_for_added_file`，还是“已有 AI 文件内部仍有未覆盖尾段”。本次涉及代码文件包括 `src/commands/checkpoint_agent/presets/github_copilot/{mod.rs,ide.rs,cli.rs}`、`src/commands/checkpoint_agent/orchestrator.rs`、`src/daemon/checkpoint.rs`、`src/authorship/post_commit.rs`；新增回归覆盖 `dirty_files_are_only_a_fallback_when_payload_has_paths`、`ai_pre_edit_request_requires_agent_marker_and_will_edit_role`、`ai_pre_edit_request_rejects_human_agent_tool`、`human_agent_tool_metadata_is_not_unclosed_ai_pre_edit`、`ai_pre_edit_path_evidence_requires_matching_ai_edited_tool_use`、`ai_pre_edit_path_evidence_rejects_human_agent_tool`、`ai_pre_edit_path_evidence_rejects_bash_and_plain_human`。需要注意：这类客户端修复只保证新版本后续提交产生更完整证据；已经上传的 `87ded2d` 历史记录不会被自动重写，若要修历史看板，必须另走明确的历史数据修复/重传流程。

> **实施补充（2026-06-18，王浩亮 `united-learning-association` 15:26:53 `RoleList.vue` 未归 AI 的现场结论与诊断增强）**：针对 `logs/王浩亮.jsonl` 中 `2026-06-18 15:26:53` 左右 `src/views/systemSettings/RolePermissionSection/RoleList.vue` 未识别为 AI 的问题，确认这不是远端看板丢记录，也不是上传 HTTP 失败。对应本地提交 `fb6a8e3feab8f714d78ae342f7ce48679fa8d4e5` 已经自动上传成功，payload 明确是 `gitDiffAddedLines=9`、`aiAdditions=0`、`humanAdditions=1`、`unknownAdditions=8`，并且 `upload_stats_http_response_received.statusCode=200` / `upload_stats_succeeded` 均出现。根因仍在归因输入证据：该提交的 `post_commit_working_log_loaded.checkpointSummary` 显示 `aiCheckpointCount=0`、`aiEntryCount=0`，只有 `human=1`、`known_human=4`，且所有 `uniqueFileSample` 都是 `RoleList.vue`；15:22 至 15:26 之间日志里能看到多次 `checkpoint known_human --hook-input stdin`、`daemon_checkpoint_request_resolved_files` 成功解析到 `RoleList.vue`，但没有任何 `checkpoint github-copilot`、`ai_agent` 或 Copilot tool path 证据。因此本次 8 行 `unknown` 不能凭用户确认事后硬改成 AI，只能判定为“Copilot/AI 采集入口没有为这个文件留下证据”。

> 这条现场还暴露出诊断日志不足：旧 `post_commit_attribution_gap_detected` 只在“全部新增行 unknown”时记录，像 `fb6a8e3` 这种 `humanAdditions=1`、`unknownAdditions=8` 的部分未归因不会有 gap 事件，排查必须手工组合 `statsSummary` 和 checkpoint summary。当前 `src/authorship/post_commit.rs` 已把诊断扩展为 `attribution_gap_reason(...)`：只要本次新增行存在 `unknownAdditions > 0`，就记录 `post_commit_attribution_gap_detected`；全 unknown 写 `reason=all_added_lines_unknown`，部分 unknown 写 `reason=partial_added_lines_unknown`。这不改变 AI/人工/unknown 统计结果，只增加可观测性；新增回归 `test_attribution_gap_reason_distinguishes_all_and_partial_unknown` 覆盖王浩亮这种“部分 unknown”场景。后续若同类问题再次出现，应先看该事件中的 `checkpointSummary.aiCheckpointCount`、`promptSummary.tools`、`pathspecCount` 和 `statsSummary.unknownAdditions`，确认是 AI checkpoint 缺失、路径解析缺失，还是同文件尾段未覆盖。

> **实施补充（2026-06-18，王江龙 `ltc-platform` 18:13:50 三个 `application-*.properties` 人工新增落入 unknown 的根因修复）**：针对 `logs/王江龙-20260618.jsonl` 中提交 `4765dd13959e8f626c04f8e84795ab97496970ac`，`src/main/resources/application-dev.properties`、`application-test.properties`、`application-uat.properties` 实际为人工编写，但最终既没有识别成 AI，也没有识别成人工。现场先排除远端看板和上传：`post_commit_stats_computed` 本地已经给出错误统计，`gitDiffAddedLines=181`、`aiAdditions=147`、`humanAdditions=18`、`mixedAdditions=3`、`unknownAdditions=16`。提交时 `post_commit_working_log_loaded` 里三份 properties 只出现在 `human` kind，`known_human` 只有 `CrAiParseFlowServiceImpl.java`；`post_commit_ai_attribution_gaps_filled.excludedCandidateFileSample` 也明确把三份 properties 排除，说明它们不是 AI gap-fill 漏补，而是没有强人工 attestation。

> 具体事件链是：17:37:24 Copilot `PreToolUse` 对三份 properties 和一个 Java 文件发出 `CheckpointKind::Human + pathRole=WillEdit`，同一个 `toolUseId=call_RaqVpP8JXC61TKlstDWmkOcK__vscode-1781745112279`；17:37:34 对应 `PostToolUse` 返回 `bash_post_checkpoint_resolved.action=no_changes`、`finalFileCount=0`。旧逻辑把前置 `WillEdit` 当作一个未关闭的 AI pre-edit 基线，但 `no_changes` 只写调试日志，没有把同 `tool_use_id` 的前置状态关闭。与此同时，VS Code 扩展曾用“最近看到 Copilot chat-editing 文档”来跳过下一次保存，daemon 队列层又会把 pending AI 文件从 `KnownHuman` 请求里过滤掉；因此后续三份 properties 的真实人工保存证据可能在客户端或队列层被吞掉。到 commit 时只剩弱 `human` 基线，没有 `h_*` 人工作者证据，按审计语义只能算 `unknown`。

> 本轮修复是行为修复而不是只补日志：`agent-support/vscode/src/known-human-checkpoint-manager.ts` 不再因为看到 Copilot chat-editing 文档就跳过保存，人工保存仍会发送 `known_human`；`src/daemon.rs` 移除队列层 pending-AI 文件过滤，避免整批 `KnownHuman` 被提前丢弃；`src/commands/checkpoint_agent/bash_tool.rs` 在 `PostToolUse no_changes` 时向 daemon 发送 `ai_pre_edit.close`，`src/daemon/control_api.rs` / `src/daemon/checkpoint.rs` 会在同一 family sequencer 中为匹配 `tool_use_id` 写入 `ai_pre_edit_closed=true` 的 close marker，只关闭基线、不生成 AI 或人工新增归因；`KnownHuman` 降级也改为逐文件处理，只有仍处于未关闭 pre-edit 的文件才走弱归因，同一个请求里的其他人工文件仍可产生 `h_*`。

> 新增回归 `test_no_changes_close_allows_later_known_human_properties_attribution` 复现三份 properties 被 Copilot `WillEdit` 预声明、`PostToolUse no_changes`、随后人工保存再提交的形态，断言最终 `human_additions=3`、`unknown_additions=0`。本轮继续补上一个被复盘暴露的根因：`src/commands/checkpoint_agent/orchestrator.rs` 之前会给所有 `PreFileEdit` 无条件写入 `ai_pre_edit=true`，而 `agent-v1 type=human` 也会解析成 `PreFileEdit + AgentId(tool=human)`；这会让人工前置编辑在 daemon 里被当成未关闭 AI pre-edit，进而压制后续 KnownHuman 或参与 AI path evidence。现在 orchestrator 只对非 `human/known_human` 的 AI 工具上下文写 `ai_pre_edit` 元数据，`src/daemon/checkpoint.rs` 和 `src/authorship/post_commit.rs` 也会防御性拒绝 `agent_tool=human/known_human` 的历史脏标记。相关单元测试覆盖 `ai_pre_edit_close_clears_matching_tool_use`、`ai_pre_edit_close_does_not_clear_other_tool_use`、`entries_for_unclosed_ai_pre_edit_close_inherits_prior_attribution`、`ai_pre_edit_request_rejects_human_agent_tool`、`human_agent_tool_metadata_is_not_unclosed_ai_pre_edit`、`ai_pre_edit_path_evidence_rejects_human_agent_tool`；集成回归新增 `test_agent_v1_human_pre_edit_does_not_register_pending_ai_edit`。历史已经上传的 `4765dd1` 不会被客户端新版本自动改写；若要修历史看板，需要明确执行历史 note 重算和重传。

> **实施补充（2026-06-15，Windows 手动安装指定旧版本后的入口版本分裂修复）**：针对管理员 PowerShell 执行 `https://github.com/rj-gaoang/git-ai/releases/download/v2.2.32/install.ps1` 后输出“Successfully installed git-ai into `C:\Users\admin\.git-ai\bin`”，但同一窗口 `git-ai --version` 仍可能显示 `2.2.33` 的现象，本轮确认这不是 2.2.32 二进制下载失败。现场文件状态是：`.git-ai\bin\git-ai.exe` 已经被旧 GitHub release 安装器写成 `2.2.32`，但 `.git-ai\launcher\git-ai.exe` 仍是 `2.2.33`，`.git-ai\current-exe` 也仍指向 launcher；同时 machine PATH 中 `.git-ai\launcher` 位于 `.git-ai\bin` 前面，不同 PowerShell 会话 / PATH 合并顺序会解析到不同入口。根因是旧手动安装器只维护 legacy `.git-ai\bin`，而 windows-update-service 和当前安装完整性检测已经把 `.git-ai\launcher` / `current-exe` 作为权威入口，导致“bin 是 2.2.32、launcher 是 2.2.33”的半安装状态。修复方案落在 `git-ai/install.ps1`：Windows 直接安装也改为先把下载并校验后的二进制安装到 `.git-ai\launcher\git-ai.exe`，原子写入 `.git-ai\current-exe` 指向 launcher，再从 launcher 同步一份兼容副本到 `.git-ai\bin\git-ai.exe`，`exchange-nonce`、`install-hooks` 和 daemon restart 都优先使用 launcher；`.git-ai\bin` 继续保留给既有 PATH 与旧 agent hook 配置，避免破坏历史安装。新增回归测试 `windows_install_script_synchronizes_launcher_current_exe_and_compat_bin` 和 `windows_install_script_writes_current_exe_pointer_to_launcher`，真实 PowerShell 安装测试断言 launcher、current-exe、compat bin 三者同时存在且 launcher / bin `--version` 完全一致。需要注意：已经发布出去的 `v2.2.32 install.ps1` 不能靠当前源码自动改变，除非重新替换该 release asset；当前修复覆盖后续发布版本，并可通过重新运行修复后的安装脚本把本机入口分裂修回一致。

> **实施补充（2026-06-15，`-BestEffort` 探活脏数据彻底切断）**：针对看板仍出现 `author=-BestEffort`、`customAttributes.source=-GitPath` 的安装成功测试数据，本轮确认直接根因不是 git-ai Rust 探活，而是 Windows Update Service 的旧外部探活链路。wrapper 使用 `$scriptArgs = @('-GitAiExe', $GitAiExe, '-Source', 'installTest', '-BestEffort', '-GitPath', ...)` 后执行 `& $scriptPath @scriptArgs`，这里 PowerShell 数组 splatting 并不会按命名参数绑定，而是把数组元素按位置传给脚本；于是 `-BestEffort` 可能落入 `UserId`，后续变成 `author=-BestEffort` / `X-USER-ID=-BestEffort`，`-GitPath` 可能落入 `Source`，后续变成 `source=-GitPath`。同时 service 默认关闭安装脚本内置 Rust 探活，再由 `upload-install-test.ps1` 直连同一个看板接口，因此 git-ai 本身正常也会被旧外部 producer 的脏数据污染共享看板。修复分三层：第一，Windows Update Service 默认不再设置 `GIT_AI_SKIP_INSTALL_TEST_UPLOAD=1`，也不再默认执行外部 `upload-install-test.ps1`，安装成功探活以 git-ai Rust 内置 `installTestProducer=git-ai-rust` 为权威；第二，外部脚本仅在显式设置 `GIT_AI_SERVICE_RUN_LEGACY_INSTALL_TEST_UPLOAD=1` 时作为兼容入口运行；第三，兼容入口改用 hashtable 命名参数 splatting，且 `upload-install-test.ps1` 自身拒绝 `-BestEffort`、`-GitPath`、`-Source` 这类参数形 user/source 候选，避免再进入 `author`、`source`、`X-USER-ID`。线上 `172.16.37.100:8888/ruijie-ai-update-service/upload-install-test.ps1` 当前仍是旧哈希 `8613F17D...`，本地修复版为 `7904EDC5...`；必须发布新的 Windows Update Service 包和脚本后，后续用户才不会再产生这类 `BestEffort` 探活数据。历史数据应按 `installerScript=upload-install-test.ps1` 或 `author/source` 以 `-` 开头隔离。

> **实施补充（2026-06-15，git-ai 安装器保证自身 Rust 探活不被 service 环境误关）**：为避免已有或旧版 Windows Update Service 在父进程环境中遗留 `GIT_AI_SKIP_INSTALL_TEST_UPLOAD=1` 导致 `git-ai install-hooks` 里的 Rust 探活被跳过，`git-ai/install.ps1` 现在不再裸调用 `& $launcherExe install-hooks`，而是通过 `Invoke-GitAiInstallHooks` 包一层环境隔离：调用前仅在非测试数据库环境下临时移除 `GIT_AI_SKIP_INSTALL_TEST_UPLOAD`，调用结束后恢复原值。这样 `ruijie-ai-update-service` 仍可正常安装、校验和记录自身状态，但安装成功数据由 git-ai 自身的 `installTestProducer=git-ai-rust` 链路发送；`GIT_AI_TEST_DB_PATH` / `GITAI_TEST_DB_PATH` 测试环境仍保持不上传。

> **实施补充（2026-06-16，安装探活作者 `git-ai installer` 污染兜底修复）**：针对看板探活行显示 `git-ai installer / git-ai-install / install-success` 的疑问，本轮把字段语义和真正异常拆开确认：`projectName=git-ai-install`、`branch=install-success`、`source=git-ai-install` 是安装成功探活的 synthetic 维度，用来让看板识别“这是安装成功探活，不是真实项目提交”，不是异常；异常是 `commits[].author=git-ai installer`，因为作者字段必须是真实用户 ID、Git 邮箱或 IP，不能是工具名。当前 git-ai Rust 内置探活正常会从 `GIT_AI_REPORT_REMOTE_USER_ID` / IDE MCP `X-USER-ID` / Git 邮箱 / IP 解析身份，本机日志也显示 `userIdentitySource=ide_mcp_config`，MCP 配置值为真实 `108`；而旧 `windows-update-service/upload-install-test.ps1` 兼容脚本仍保留 `author = ResolvedAuthor || 'git-ai installer'` 的兜底，一旦没有解析到用户或旧外部探活仍被启用，就会直接把“安装器”写进看板作者。修复方案分两层：git-ai Rust 探活新增 reserved identity 拒绝规则，凡是 `git-ai installer`、`git-ai-installer`、`git-ai-install`、`install-success`、`upload-install-test.ps1`、`windows-update-service-legacy` 等探活/工具标签出现在环境变量或 MCP 身份里，都会记录 `install_test_identity_rejected(reason=reserved_install_probe_label)` 并继续回退到真实 MCP / Git 邮箱 / IP；legacy PowerShell 探活脚本也不再用 `git-ai installer` 兜底，改为真实用户、IP，最后才写明确的 `unknown-install-user`。后续看板判定规则应按 `installTestProducer=git-ai-rust` / `installerScript=git-ai-rust-install-test` 信任 git-ai 内置探活；历史 `author=git-ai installer` 且 `installerScript=upload-install-test.ps1` 或 `installTestProducer=windows-update-service-legacy` 的记录应作为旧外部探活脏数据隔离。

> **实施补充（2026-06-16，王治尧安装成功无探活 / 11:57 提交无上传修复）**：结合 `logs/王治尧 (2).jsonl` 排查，安装窗口附近只看到 `install-hooks` 的 `process_started`，没有后续 `install_test_upload_*`；11:57 提交窗口也只有 `blame` / `checkpoint`，没有 `git_proxy_entered`、`post_commit_*` 或 `upload_stats_*`。这说明问题发生在上传 HTTP 之前：安装脚本认为安装成功，但 git-ai 的 hook/trace2 收尾没有完整生效，后续真实 `git commit` 没有稳定进入 git-ai proxy 或 daemon post-commit 链路。直接薄弱点有两个：PowerShell 调 native exe 时非 0 `$LASTEXITCODE` 不会稳定进入 `catch`，旧 `Invoke-GitAiInstallHooks` 没检查退出码，可能把失败的 `install-hooks` 打印成成功；安装探活又绑在 `install-hooks` 末尾，一旦 `install-hooks` 提前失败，探活完全没有独立兜底。修复落在 `git-ai/install.ps1` 和命令入口：新增内部 `git-ai post-install-probe`，安装脚本在 hook 设置成败之后都会单独调用它；`Invoke-GitAiInstallHooks` 和 `Invoke-GitAiPostInstallProbe` 都显式检查 `$LASTEXITCODE`，避免“显示成功但实际没装好”；`install-hooks` 自身也把探活收尾放到 async 安装结果解包之前，即使某个 installer 返回 fatal error，也先尝试写安装成功探活；安装类命令加入 root/Administrator guard 豁免，避免管理员 PowerShell 安装环境被普通命令保护逻辑干扰。探活仍复用 `install_test_uploads.json` marker，同版本、同身份、同 endpoint 不会重复上报。

> **实施补充（2026-06-16，`install-hooks` 非核心前置失败降级）**：继续收敛 `install-hooks` 自身失败面后确认，trace2 配置、旧 `core.hooksPath` 修复、daemon 重启和各 IDE / agent hook 安装已经按非致命 warning 或 per-tool 状态处理；剩余会让整个命令退出的前置动作主要是当前 binary 路径 canonicalize 以及安装时把 `API_BASE` / `API_KEY` 写入 `~/.git-ai/config.json`。这些动作不是 hook / trace2 的核心闭环，若用户本机 config JSON 损坏、目录短暂不可写或 Windows exe 路径 canonicalize 失败，不应该阻断 `install-hooks`。当前修复把当前 binary 路径解析加 fallback，并把 `persist_install_config` 失败降级为 warning；即使配置持久化失败，`install-hooks` 仍继续执行 hook 安装、trace2 配置和安装探活。新增回归测试覆盖已有坏 `config.json` 且设置 `API_BASE` 时，best-effort 配置持久化会吞掉该非核心错误，避免命令在 hook / trace2 安装前退出。

> **实施补充（2026-06-16，安装探活区分最终成功与安装失败）**：安装探活语义收紧为“结果探活”而不是“进程到达 git-ai 探活”：`installStatus=success` 只允许在完整安装脚本收尾成功后发送；`install-hooks` 被 `install.ps1` 调用时通过 `GIT_AI_DEFER_INSTALL_HOOKS_PROBE=1` 延后自身探活，避免 hook 阶段成功被误认为整体安装成功。若 hook 阶段或安装器明确失败，会发送 `installStatus=failed`、`branch=install-failed`，并携带 `installFailureStage` / `installFailureReason`，看板可以直接区分“安装成功”和“安装失败”。手工运行 `git-ai install-hooks` 时仍会在命令结果确定后发送对应 success/failed 探活。marker key 和本地 `install_test_uploads.json` 也加入安装状态与失败阶段/原因，避免同版本同用户的失败探活被成功探活去重或反向污染。

> **实施补充（2026-06-17，6 月 16/17 归因异常、Copilot `dirtyFiles` 路径兜底、recent `KnownHuman` 保存误归因与自动更新不丢本次 commit）**：本轮把张艺锋、庞福泽、徐建丰、张彪、胡晴琨等现场日志中暴露的归因异常统一归档。日志治理层面，按用户要求只保留目标日期窗口，`徐建丰.jsonl` / `张彪.jsonl` 仅保留 `2026-06-16`，`胡晴琨.jsonl` 仅保留 `2026-06-17`，过滤前文件以 `.bak-before-...-filter` 形式保留，异常大日志和旧脏日志不再参与本轮判断。归因层面，`op-api` 在 `2026-06-17 16:29:36` 的提交 `23a81b7cc8a45fbc849630b957681f884b856c5f` 使用的是 `2.2.39`，不是旧版本问题；日志显示 Copilot hook 的 `tool_input` / `tool_response` 有时只包含文本片段、执行结果或被脱敏内容，没有 `file_path/path/files` 字段，真实文件路径在顶层 `dirtyFiles` / `dirty_files` 的 key 中。旧解析只把 `dirtyFiles` 当作内容快照，不把 key 当路径兜底，导致 `checkpoint github-copilot` 路径为空并跳过；现已在 GitHub Copilot `ide` / `cli` preset 中加入 `file_paths_from_dirty_files(...)` 兜底，覆盖 `rj-ltc-contract-web`、`ai-cr-manage-service`、`op-api` 这类“代码实际由 AI 写，但大量文件 unknown”的路径缺失场景。另一个独立问题是 AI 写入后会触发 VS Code / IDE 的保存或文件变更监听，`AI Edited` 后紧跟 `IDE known_human` 是两个独立 hook 的正常竞态，并不代表用户手工重写了整文件；旧逻辑可能让这次保存事件把已有 AI 行重新抢成人工。当前 `checkpoint.rs` 已在 previous state 中保留时间戳，1 秒内继续拒绝紧随 AI 的 `KnownHuman`，30 秒内的 recent AI save 只归因真实新增 / 改动行，已有 AI 行保持 AI。自动更新层面，`git commit` 成功后现在先启动本次 commit 的 fallback `upload-stats --wait-for-authorship-note-ms 5000 --skip-if-already-uploaded --acquire-activity-lock-before-stats`，再调度后台自更新；后台升级仍正常执行，但不会先重启 / 替换 runtime 导致远程看板丢失本次 commit 的归因记录。

> **实施补充（2026-06-17，王江龙 ltc-platform 同文件 AI 尾段 unknown 修复）**：针对 `logs/王江龙-20260617.jsonl` 中 `2026-06-17 22:00:44` 左右提交 `819ae3f1318750b4bfeac86d7dac8a0461ec2f65`，`src/main/java/com/ruijie/ltc/service/impl/CrAiParseFlowServiceImpl.java` 本次新增 298 行却只识别 159 行 AI、剩余 139 行既非 AI 也非人工的问题，确认不是服务端看板映射错误。日志显示 post-commit 已加载 16 个 Copilot AI checkpoint，整体 `gitDiffAddedLines=592`、`aiAdditions=453`、`unknownAdditions=139`；同时 `post_commit_ai_attribution_gaps_filled` 只补了 `missingAddedLineCount=10`，说明旧 gap-fill 只覆盖“AI 会话生成但完全没进 pathspec 的漏文件”，没有覆盖“文件已经进入 AI pathspec，且已有部分 AI line range 落地，但同一文件尾部还有未被 line range 覆盖的新增行”。现场 `CrAiParseFlowServiceImpl.java` 正好表现为同一文件前 159 行已有 AI attestation、后 139 行缺 line-range attestation。修复落在 `post_commit` 的 AI gap-fill：在已有当前 AI attestation 落地的文件内，收集本次 commit 新增行里仍未被任何 human/AI attestation 覆盖的行，并在 `totalAiAdditions >= 已落地 AI 行 + 待补缺口行` 的预算保护下补到当前 AI attestation；完全没有 AI 落地证据的文件仍不会被事后硬判为 AI。新增诊断字段 `unattributedAddedLineCount` / `landedAiFileCount`，用于区分“漏文件 pathspec 缺口”和“已识别 AI 文件内部缺口”；新增回归 `test_copilot_ai_session_fills_unattributed_tail_in_checkpointed_file` 覆盖 298 行中 159 行已归 AI、139 行尾段缺失的形态。

> **实施补充（2026-06-15，Windows Update Service 安装 git-ai 时 busy launcher 不再阻塞升级）**：针对 `ruijie-ai-update-service` 安装 / 重试 `v2.2.35` 时持续失败，现场日志 `C:\ProgramData\RuiJieAIUpdateService\logs\install-v2.2.35-20260615154604.log` 显示失败发生在 wrapper 的 pre-install cleanup 阶段：`C:\Users\admin\.git-ai\launcher\git-ai.exe` 正被 `git-ai --version`、`git-ai checkpoint codex --hook-input stdin`、`git-ai bg run` 等进程持有，wrapper 等待 `launcher\git-ai.exe` 可独占写打开超时后直接抛出 `pre-install git-ai launcher cleanup failed; launcher_files_ok=False`，导致真正的 git-ai 安装器根本没有机会执行。根因不是 service 没有先停止自身，也不是用户必须手工杀进程，而是 wrapper 把“launcher exe 当前 busy”当成安装前置硬失败；但 Windows 支持先把运行中的 exe 重命名到 retired 路径，再把新 exe 放回稳定路径，git-ai 安装器本身已经具备这种替换能力。修复分两层：第一，`windows-update-service/src/main.rs` 的 `Invoke-PreInstallCleanup` 不再等待 launcher/bin 文件空闲，也不再因为 `launcherFilesOk=false` 退出；它只记录 `pre_install_cleanup_launcher_files_busy_non_blocking` / `pre_install_cleanup_legacy_non_blocking` 诊断，然后让下载后的 git-ai 安装器接管替换。第二，`git-ai/install.ps1` 的 `Install-BinaryWithRenameFallback` 改为优先 rename-retire 旧 exe，只有退休失败才进入停止 daemon / 杀同目录进程树兜底；同时 PATH 更新改为优先 `.git-ai\launcher`，`.git-ai\bin` 只保留兼容副本，减少新老入口混跑。service wrapper 还会 patch 旧版远程安装脚本，给旧 `install.ps1` 注入 `prefer_rename_retire_before_process_kill` 策略，避免服务端 git-ai 包未同步时仍回到先杀进程的旧行为。该方案不会在普通命令路径做全系统进程扫描，也不会为了升级频繁杀用户 IDE / hook 进程；只有替换旧 exe 的 rename-retire 极少数失败时才走进程清理兜底。验证已覆盖 wrapper 生成脚本定向测试 `cargo test test_build_installer_command_includes_wrapper_diagnostics --manifest-path windows-update-service\Cargo.toml -- --nocapture`、release 构建 `cargo build --release --manifest-path windows-update-service\Cargo.toml`、本地发布包生成 `windows-update-service\prepare-server-package.ps1 -KeepOutput`。本地新包已生成在 `windows-update-service\dist\ruijie-ai-update-service`；线上 `http://172.16.37.100:8888/ruijie-ai-update-service/manifest.json` 仍是旧包，必须发布新包后用户重试安装才会进入该修复路径。

> **实施补充（2026-06-12，大提交自动上传补算 stats）**：针对 `debug.jsonl` 中 `post_commit_stats_skipped(reason=expensive_commit)` 后又出现 `upload_stats_skipped(reason=stats_unavailable)` 的链路，`git-ai` 自动上传不再因为 post-commit 快路径未携带 `CommitStats` 而直接跳过。大提交仍然保留提交钩子内的昂贵统计保护，但 `post_commit` 会把同一份 ignore patterns 和“允许补算”标记传给上传模块；上传任务在构建 payload 时重新调用 `stats_for_commit_stats(...)` 补齐统计后再上传。merge commit 的 stats 缺失仍保持跳过，避免把合并提交误上传为 0 行统计。新增诊断字段 `statsSource=background_recompute`、`willRecomputeStats=true`、`willRecomputeMissingStatsForUpload=true` 用于确认该路径已经进入补算。

> **实施补充（2026-06-13，弱 KnownHuman 保存不再强归人工）**：针对王治尧机器 `2026-06-12T10:38:52` 提交 `cabbb35a` 中 `gitDiffAddedLines=1576`、`humanAdditions=900`，但现场确认代码全部由 AI 编写的误归因链路，`git-ai` 已调整 checkpoint 处理：当同一 base commit 的 working log 中已经存在 AI checkpoint / 非人工归因，而后续 `KnownHuman` 保存事件缺少明确编辑器元数据（例如没有 `kh_editor`，常见于 AI 工具调用后 IDE 自动保存或弱来源保存事件）时，不再把它作为强人工 attestation 写入 `h_*`，而是降级为普通 `Human` checkpoint。这样它不会覆盖已有 AI 归因，也不会把不确定的大块新增代码误报成人工；真实带编辑器元数据的 KnownHuman 仍保持原有人工归因语义。新增回归测试 `weak_known_human_save_after_ai_does_not_create_human_additions`，同时保留 `test_stats_for_mixed_commit` 验证真实人工追加仍能计入 `humanAdditions`。另 `2026-06-12T14:29:56` 提交 `750ef1d` 属于大提交 `expensive_commit -> stats_unavailable` 链路，已由前述“大提交自动上传补算 stats”修复覆盖。

> **实施补充（2026-06-14，安装测试上传、无 note 兜底、北京时间与安装生效闭环）**：本轮现场把“源码已修”和“用户机器真实生效”拆成独立闭环处理。上传协议层面，安装成功后的测试上传、commit 后自动上传、手动 `upload-stats` 回补共用同一套远程看板 schema，并都写入 `~/.git-ai/logs/debug.jsonl`。安装测试上传不能因为 MCP `X-USER-ID` 缺失而跳过，身份顺序为 MCP / 环境用户 ID、Git 邮箱、IP 地址，且不得伪造测试用户；payload 至少包含 git-ai 版本、Git 版本、操作系统和版本，并补齐后端必填的 `model=unknown`。自动 / 手动上传遇到没有 `refs/notes/ai` 的 commit 时，不再跳过，而是发送 `hasAuthorshipNote=false` 的 metadata-only payload，把新增行计入 `unknownAdditions`，不凭空制造 AI 或人工归因。所有上传时间统一先解析原始 `%aI` 时区，再转换为北京时间 `yyyy-MM-dd HH:mm:ss`，不能上传 RFC3339 纳秒格式，也不能只截字符串导致 `05:13` 误报成北京时间。现场 `bfd32c8` 未自动上传的直接根因最终确认为全局 `core.hooksPath` 指向不存在的旧 `yoavlax.ai-contribution-tracker\git-hooks` 目录，Git 提交阶段根本没有进入 post-commit hook；修复后 `install-hooks` 会自动清理失效的历史 hooksPath，并在 Windows 下重启 daemon 前兜底清理同安装目录残留的 `git-ai.exe bg run`，即使 `daemon.pid.json` 缺失也不能让旧进程继续接收 Trace2 / wrapper 事件。现场验证要求同步检查源码、release 构建、`%USERPROFILE%\.git-ai\bin\git-ai.exe`、`git.exe` 哈希、Trace2 pipe、`core.hooksPath` 和 daemon 进程，避免再次出现“安装了新版本但旧进程还在跑”的误判。

> **实施补充（2026-06-14，Windows 安装器 busy exe 替换闭环）**：针对 `v2.2.30` Windows 安装日志反复打印 `Waiting for file to be available: C:\Users\admin\.git-ai\bin\git-ai.exe` 与 `Stopping lingering git-ai processes: ...` 的问题，根因不是单台机器需要手工清理进程，而是安装器把“运行中的 exe 不能用独占写句柄打开”当成必须等待的失败条件；同时旧清理逻辑只对枚举到的 PID 执行 `Stop-Process`，不会杀完整进程树，VS Code / Copilot hook 残留的 `checkpoint ... --hook-input stdin` 子进程或旧 `bg run` 子进程仍可能继续持有旧 `git-ai.exe` 镜像。`taskkill /T` 输出中“成功终止的是子进程，父 PID 又显示没有实例在运行”正好说明旧安装器枚举到的 blocker 和真实占用者之间存在父子进程 / 瞬时进程错位。修复方案落在 `git-ai/install.ps1`：新增 `Install-BinaryWithRenameFallback`，替换 `git-ai.exe` 时不再一直等待可写句柄；若直接覆盖失败，先尝试关闭 daemon 与同安装目录进程树，再把旧 `git-ai.exe` 重命名为同目录 `git-ai.exe.retired-<timestamp>-<pid>`，立即把新二进制放回稳定路径，旧文件能删则删，删不掉则通过 `MoveFileEx(..., MOVEFILE_DELAY_UNTIL_REBOOT)` 登记重启后删除。`Stop-GitAiManagedProcesses` 同步改为调用 `taskkill.exe /F /T /PID`，避免只杀父进程留下真正锁文件的 hook 子进程；legacy `git.exe` wrapper 禁用也改为优先重命名，避免老用户迁移时在同类文件锁上再次卡住。验证使用隔离 HOME 的真实 PowerShell 安装脚本测试，而不是修当前机器：`cargo test --test windows_script_checks -- --nocapture` 已覆盖新用户安装、老 wrapper 禁用、daemon 运行中重装、`GIT_AI_DEFER_IF_BUSY=1` passive update 下旧 `git-ai.exe` 仍运行但安装成功并退休旧 exe 的场景。

> **实施补充（2026-06-14，张一峰 op-api 归因误判 / 漏判修复）**：针对 `logs/zyf-20260614.jsonl` 中两条已确认“实际均为 AI 生成”的异常提交补齐根因和修复。其一，`2026-06-01 15:19:37` 左右的 `op-api` 提交 `e2cc766`，日志显示 `promptCount=0`、`sessionCount=0`、`checkpointCount=1`，最终却得到 `humanAdditions=346`、`aiAdditions=0`、`unknownAdditions=7`；根因是缺少真实 AI checkpoint / prompt 时，IDE 自动保存类的弱 `KnownHuman` 事件被当成强人工 attestation 写入 `h_*`，把整块新增代码误报成人工。现在 `KnownHuman` 必须带有效 `kh_editor` 元数据才保留强人工语义；缺少编辑器元数据时降级为普通 `Human` checkpoint，不再产生 `h_*` 归因，也不会凭空把未知新增行算成人工。其二，`2026-06-11 13:58:10` 左右的 `op-api` 提交 `991aca4`，`src/main/java/com/ruijie/op/api/controller/JobTask.java` 实际为 AI 生成，但统计只覆盖了约 150 行 AI，其余落到 unknown；根因是 Copilot terminal / bash 工具调用在缺失 pre snapshot、snapshot 失败或 hook 超时时，旧链路直接放弃 post checkpoint，导致部分 AI 改动没有落入 authorship note。现在 `PostBashCall` 会在 `MissingPreSnapshot` / `SnapshotFailed` / `HookTimeout` / post-hook error 时回退到 `git_status_fallback` 提取实际变更文件，并继续发送 AI checkpoint。新增诊断事件 `bash_post_checkpoint_resolved`、`checkpoint_request_send`、`checkpoint_request_started`、`checkpoint_request_finished`，用于确认路径解析、daemon 接收和 checkpoint 落盘是否完整。对应回归测试包括 `weak_known_human_without_ai_checkpoint_does_not_claim_human_additions` 与 `test_run_in_terminal_missing_pre_snapshot_uses_git_status_fallback`。

> **实施补充（2026-06-15，op-return-exchange 非 UTF-8 文本文件漏归因修复）**：针对 `D:\rj-op\op-return-exchange` 最新提交 `cf060485afcfdacc4d0c85b0ee9902e08798fa3e` 中 `OpReturnExchangeApplicationTests.java`、`SplitTests.java`、`MailTest.java` 等文件实际由 Copilot 追加测试方法但统计落入 `unknownAdditions` 的问题，日志确认不是 Copilot 未检测到路径：`bash_post_checkpoint_resolved` 已检测 90 个文件，`daemon_checkpoint_request_resolved_files` 却只解析出 79 个文件，差集中的 11 个 Java 源文件每个本次提交正好新增 5 行，用户点名的 3 个文件均在差集中。根因是 checkpoint 构造阶段使用 `fs::read_to_string(...).ok()` 读取整文件；这些历史文本文件包含非 UTF-8 字节，读取失败后 `CheckpointFile.content=None`，daemon 侧旧逻辑又只保留带 `content` 的文件，导致“路径已发现但内容快照为空”的显式 AI 编辑被静默过滤，最终 post-commit note 缺少这些文件的 attestation。修复方案是把 checkpoint 文件读取改为字节读取并排除 NUL 二进制文件，文本内容用 `String::from_utf8_lossy` 形成稳定快照；daemon 解析请求时也增加 workdir 兜底读取，避免旧客户端或其他插件传入 `content=None` 时再次丢文件。该修复位于通用 checkpoint 文件读取链路，不依赖 Java 扩展名；新增回归测试 `test_run_in_terminal_non_utf8_existing_text_files_are_attributed`，同时模拟含非法 UTF-8 字节的 `.java` 与 `.properties` 既有文本文件经 Copilot terminal fallback 追加内容，提交后断言新增行全部计入 AI 且 `unknown_additions=0`。

> **实施补充（2026-06-15，Windows busy exe 方案缺口与运行时收敛修复）**：`2.29` 的 retired binary 替换策略是必要兜底，但它只能解决“安装器能否替换被占用的 `git-ai.exe`”，不能解释也不能根治“为什么持续出现很多 `git-ai.exe`、为什么提交后 70 多秒才进入 `post_commit_started`”。本轮从 `C:\Users\admin\.git-ai\logs` 与真实进程状态确认，最新提交上传本身约 2.7 秒完成，HTTP 约 108ms，慢点发生在 post-commit 事件到达前；同时 Windows 上 `daemon_is_up(...)` 对 `\\.\pipe\...` named pipe 先做 `.exists()` 判断会恒为 false，导致健康 daemon 被误判为离线，从而重复尝试启动；`checkpoint ... --hook-input stdin` 在宿主未关闭 stdin 时会无限等待，形成持有旧 exe 镜像的残留进程；`active-runtime.json` 指向的 replacement runtime 如果已经失效，客户端仍可能继续路由到废弃 runtime；测试隔离还曾把 `git-ai-test-home-*` 临时 bin 目录泄露到真实 PATH，放大进程风暴。修复方案不是在每次用户命令里全系统扫描 / 杀进程，而是分层收敛：Windows daemon 健康检查对 named pipe 直接做 100ms 连接探测，不再用文件存在性误判；hook stdin 默认 5 秒超时，可用 `GIT_AI_HOOK_STDIN_TIMEOUT_MS` 调整，超时后以 0 退出跳过 checkpoint，避免卡住 IDE/Agent；`active-runtime.json` 只在 control/trace socket 可连接或 runtime lock 仍被持有时保留，否则自动移除并回到稳定默认 runtime；测试 harness 在 Windows 也会剔除 `git-ai-test-home-*` 和含 `git-ai.exe` 的临时 PATH。性能边界是：不做高频全进程枚举，不在普通命令路径按进程名扫机器；只在已有 active runtime 元数据时做轻量 socket/lock 探测、只在 `--hook-input stdin` 时启用超时、只在测试环境设置时清理 PATH。

> **实施补充（2026-06-15，Windows Update Service 旧探活污染隔离）**：针对看板出现 `author=-BestEffort`、`customAttributes.source=-GitPath`、`installerScript=upload-install-test.ps1`、版本仍是 `2.2.24` 的“安装成功测试”记录，本轮确认它不是当前 git-ai Rust 探活产生的记录，而是旧 `windows-update-service` 下载并执行 `upload-install-test.ps1` 后直接 POST 到同一个远程看板接口。service wrapper 曾把 `GIT_AI_SKIP_INSTALL_TEST_UPLOAD=1` 传给安装脚本，跳过 git-ai 内置探活，再以 `@('-GitAiExe', $GitAiExe, '-Source', 'installTest', '-BestEffort', '-GitPath', ...)` 调外部 PowerShell 脚本；历史版本一旦发生参数绑定 / 位置错位，PowerShell 开关就会被写入业务字段，形成 `UserId=-BestEffort`、`Source=-GitPath`、甚至 `RemoteUrl=-Source`。所以 git-ai 运行时是正常的，但它仍会“被影响”：远程看板接收的是同一套 upload schema，旧外部 producer 绕过 git-ai 直连接口，污染了共享统计数据，而不是改坏了 git-ai 的归因逻辑。git-ai 侧修复是把 Rust 内置安装探活打上不可混淆的一手标识：payload、`clientContext`、prompt `customAttributes` 和 HTTP header 均写入 `installTestProducer=git-ai-rust`、`installerScript=git-ai-rust-install-test`；身份解析链路拒绝形如 PowerShell 参数的用户值，环境变量或 MCP 中出现 `-BestEffort` / `-GitPath` / `-Source` 时写入 `install_test_identity_rejected`，无效环境变量不会遮住真实 MCP 配置，之后继续回退 Git 邮箱 / IP。看板侧应只信任 `installTestProducer=git-ai-rust` 的新探活，并隔离 `installerScript=upload-install-test.ps1` 或 `author/source` 以 `-` 开头的历史脏数据；旧 Windows Update Service 必须停止外部脚本直连，改为让 git-ai 自己发送探活，才能从源头消除外部 producer。

> **实施补充（2026-06-15，席正浩 ibs-ecp-service 10:22 归因缺失与重复探活修复）**：针对 `logs/席正浩.jsonl` 中 `ibs-ecp-service` 在 `2026-06-15 10:22:19` 左右“代码无法识别”的现场，本轮确认它不是提交统计未上传：commit `4cf9b083b3cce5d57fd572a2f4fa194fc32c0752` 已在 `10:22:30` 上传成功，HTTP `200` 且后端返回 `git-ai记录创建成功`；真正的问题是该 payload 的 `promptCount=0`、`sessionCount=0`、`attestationFileCount=0`、`aiCheckpointCount=0`，本次新增 1 行只能落入 `unknownAdditions=1`。同一用户 `10:05` 的上一笔 commit `1563dba853c31d607da7081e2d42abaa12eb08b9` 仍有 `aiCheckpointCount=2`、`promptCount=2`、`aiAdditions=24`，说明归因引擎本身能工作；断档发生在 `10:07` 自动安装 / `install-hooks` / daemon 重启 / `git-ai install success test (2.2.32)` 之后，到 `10:20-10:21` 期间只有 `git-ai blame --json --contents`，没有新的 `checkpoint github-copilot`。git-ai 不能在没有 AI checkpoint / prompt 的情况下把 unknown 硬归为 AI。第一轮修复只收敛“重复安装探活 / 自动更新扰动 / debug JSONL 坏行”这些排查噪音：`install-hooks` 的安装成功测试增加 `~/.git-ai/internal/install_test_uploads.json` 成功 marker，同一版本、同一身份、同一 endpoint 只上传一次；marker 通过 `install_test_uploads.lock` 跨进程序列化，避免并发安装重复探活；after-commit 自动更新不再无条件绕过新鲜 no-update cache，只有缓存已确认存在可用更新时才在提交后推进安装；`debug.jsonl` 写入增加 `debug.lock`，避免多个 git-ai 进程并发写入导致 JSONL 行交错，后续排查不会再被坏日志误导。这一轮不能直接恢复缺失的 AI checkpoint，直接修复见下一条。

> **实施补充（2026-06-15，Copilot checkpoint 采集前置设置自愈）**：针对“working log 没有任何 AI checkpoint / prompt，只有 human checkpoint，所以新增 1 行只能进入 `unknownAdditions=1`”这一核心问题，本轮把修复落到安装 / 自动更新后的 Copilot 采集前置条件，而不是在 post-commit 阶段篡改归因。根因边界是：`~/.copilot/hooks/git-ai.json` 只说明 hook 文件存在，不保证 VS Code 会加载；如果用户或工作区把 `chat.useHooks` 关掉，或配置了 `chat.hookFilesLocations` 但没有包含 `~/.copilot/hooks`，Copilot 编辑就不会触发 `git-ai checkpoint github-copilot --hook-input stdin`。同时，Windows 当前安装器已经不再给新用户创建 `~/.git-ai/bin/git.exe` legacy wrapper，残留的 VS Code `git.path` 如果指向不存在的 `.git-ai\bin\git(.exe)`、`git-ai-test-home-*` 临时目录或已禁用旧 shim，会让 VS Code / Git 链路命中坏路径，出现 `Cannot Run Git: No such file: C:\Users\admin\.git-ai\bin\git.exe` 一类错误，并进一步干扰真实提交链路。修复方案是让 `install-hooks` 在 VS Code 和 GitHub Copilot 安装器中都执行同一套自愈：强制把用户 settings 里的 `chat.useHooks` 设为 `true`；当 `chat.hookFilesLocations` 已存在时补齐或重新启用 `~/.copilot/hooks`；对明确属于 git-ai 且已经坏掉的 `git.path` 只做删除，让 VS Code 回到系统 Git，而不是无脑写回不存在的 wrapper。对应修改位于 `git-ai/src/mdm/utils.rs`、`git-ai/src/mdm/agents/vscode.rs`、`git-ai/src/mdm/agents/github_copilot.rs`，新增回归覆盖 Copilot hook 文件存在但 VS Code settings 关闭 / 漏配 hook 加载路径 / 残留测试 HOME `git.path` 的场景。这样后续自动安装或用户手动 `git-ai install-hooks` 后，Copilot AI 编辑会重新进入 `checkpoint github-copilot` 链路；但历史上已经提交且没有 AI checkpoint 的 commit 仍不会被事后硬判为 AI。

> **纠偏补充（2026-06-15，legacy Human checkpoint 不再落入 unknownAdditions）**：上一条说明把两个问题混在了一起，需要纠正。`Human checkpoint` 在旧实现里不是“已确认人工新增行”的强归因，而是一个 legacy baseline / sentinel；当它没有 `h_*` KnownHuman attestation 时，`VirtualAttributions` 会跳过空 entry 或剥离 `"human"` sentinel，`stats` 只能看到 git diff 新增行，却看不到任何可计入 `humanAdditions` 的 attestation，于是人工新增 1 行也会进入 `unknownAdditions=1`。这不是“没有检测到 human checkpoint”，而是检测到了以后被统计链路主动丢弃。Copilot settings 自愈只能修未来 AI checkpoint 不触发的问题，不能修复这条人工行归因。真正修复是：显式 legacy `git-ai checkpoint -- <file>` / 提交重放产生的普通 `Human` checkpoint，在没有 AI agent / 工具元数据、且不是弱 `KnownHuman` 降级时，会为本次实际新增/改写行生成 commit author 的 `h_*` 人工 attestation；AI pre-edit baseline、bash pre-hook、缺少 `kh_editor` 的弱 KnownHuman 仍不会被强归人工。新增回归 `legacy_human_checkpoint_counts_as_human_additions` 锁定 `humanAdditions=1, unknownAdditions=0`，并保留弱 KnownHuman 回归，避免再次把不确定 AI 代码误报成人工。

> **实施补充（2026-04-26）**：当前实现又追加了 4 个关键约束。
> 1. 服务端 `GitAiStatsServiceImpl.create()` 会先按 `git_ai_commit_stats.commit_code` 过滤重复 commit；同一 commit 被重复上传时，不再重复创建 summary/commit/file/tool/prompt 记录。
> 2. `git_ai_tool_stats` 一直都是 `tool` / `model` 分列存储；之前“看不到验证记录”的根因不是表结构，而是部分上传链路没有提交 `prompts[]`，而 commit 级 `toolModelBreakdown` 又可能为空。现在三条上传链路都会上传 `prompts[]`，服务端也会根据 prompt 明细回填提交级工具统计。
> 3. 新增 `git_ai_prompt_stats` 保存 prompt 明细，DDL 见 `docs/docs/plans/git_ai_prompt_stats.sql`。
> 4. `feature_flags.auto_upload_ai_stats` 默认已开启；如需显式覆盖，请使用 `GIT_AI_AUTO_UPLOAD_AI_STATS=true/false`，不要再使用 `1/0`。
> 5. `git-ai` 本身具备保存 prompt 的能力，但 prompt 的保存位置由 `prompt_storage` 决定：`default` 优先走 CAS / prompt store 并从 note 中清空 `messages`，`local` 只保存在本地 SQLite，`notes` 则把脱敏后的用户输入 prompt 直接保存在 git note 中。当前上传链路的 `promptText` 是从 note 里的 `prompts[].messages` 中 `Message::User` 记录提取的，因此在 `default` / `local` 模式下，即使 git-ai 已经保存了 prompt，服务端落库时 `prompt_text` 仍然可能为空。

> **当前快速实现决策（2026-04-26，2026-04-29 已落到代码默认值）**：如果目标是“尽快让 `git_ai_prompt_stats.prompt_text` 稳定落库”，当前最简单方案就是统一切到 `notes` 模式，而不是继续补 CAS / SQLite 回填链路。原因很直接：`notes` 模式下，`git-ai` 会在脱敏后把用户输入 prompt messages 直接保留在 `refs/notes/ai` 中，现有上传脚本和 Rust 原生自动上传都可以直接从 note 里取到 `promptText`，不需要再额外读取 `messages_url`、CAS 或本地 SQLite。因此 `git-ai` 运行时的 `prompt_storage` 缺省值已改为 `notes`；用户显式配置 `default` / `local` 时仍然按显式配置执行。

> **安装链路补充（2026-04-27）**：需求 1 已经进一步明确为“安装或更新 Spec Kit 时，都要把 git-ai 安装/更新到目标最新版本”，不再只是“缺失时安装”。这意味着 `specify init` 触发 `post-init` 时必须带 force-update 语义。另一方面，如果目标版本来自自定义 GitHub 仓库，单独提供 branch URL 还不够；自动安装最终依赖的仍然是 GitHub Releases 资产、明确 tag，或显式本地 binary。

> **实施补充（2026-04-29）**：为了让 `git_ai_prompt_stats.prompt_text` 默认稳定落库，`git-ai` 的 `prompt_storage` 默认值已经从 `default` 改为 `notes`；显式配置为 `default` / `local` 的用户仍以本机配置为准。另新增统一 debug 诊断日志：当设置 `GIT_AI_DEBUG` 后，checkpoint 归因、post-commit stats 计算和远程上传链路都会追加本地 JSONL 日志到 `~/.git-ai/logs/debug.jsonl`，用于排查“AI/人工误判”和“统计未上传”等问题；日志只记录判定、计数、路径样例、工具/模型、URL 来源、上传状态等诊断元数据，不记录代码内容或 prompt 正文。

> **实施补充（2026-04-30）**：`git-ai` 现在额外提供 `upload-stats` / `upload-ai-stats` 原生命令，显式上传本地 commit 的 authorship note + 重新计算后的 commit stats。这个命令默认只处理 `HEAD`，支持 `--dry-run`、多个 commit 和 `--source`，且不会改变 `post_commit` 自动上传、Code Review 补传或 Speckit 的 PowerShell 批量脚本行为。

> **实施补充（2026-05-03）**：GitHub Copilot VS Code native hook 的 transcript 精确回查现已兼容 hook 侧 `tool_use_id` 附带的 `__vscode-<digits>` 后缀。根因是 2026-04-29 初版 transcript fallback 只覆盖了 `tool_use_id == toolCallId` 的完全相等场景，没有覆盖 VS Code 实际运行时给 hook ID 追加后缀的情况；当 `tool_input` 被脱敏为 `...` 时，就会导致 fallback 提不出编辑路径、`AiAgent` checkpoint 缺失、最终由 `KnownHuman` 归成人工。当前实现只接受“原值相等”或“一侧去掉 `__vscode-<digits>` 后相等”，不会扫描整段会话，也不会把两个不同后缀的 hook 实例合并成同一次调用。

> **实施补充（2026-05-04）**：最新 `debug.jsonl` 又暴露出第二条独立失败路径：即使 GitHub Copilot 的 transcript fallback 已经成功提取出显式路径，`checkpoint.rs` 仍会在 `CheckpointKind::Human` + `PreparedPathRole::WillEdit` + `is_pre_commit=false` 的场景里，把当前“干净但确实存在的文本文件”按 `clean_file_without_dirty_snapshot` 直接丢弃。这样会导致显式 `will_edit_filepaths` 基线丢失，后续误归因排查时只能看到“路径已解析但被 clean 过滤掉”。当前实现已改为：只要显式路径角色是 `WillEdit`，就保留当前文本文件作为 checkpoint 基线；而 `Edited` 显式路径仍然必须依赖 dirty 状态或显式 snapshot，不会一并放宽。

> **实施补充（2026-05-04）**：同一批 `debug.jsonl` 还暴露出第三条独立失败路径：VS Code 扩展发出的 `KnownHuman` 保存 checkpoint 可能先于 GitHub Copilot 的 AI checkpoint 落盘，而旧实现对“同内容 + 最近上一条 checkpoint”只会直接判成 `unchanged_from_previous_checkpoint`，导致 AI checkpoint 永远没有机会覆盖那条抢先写入的 `KnownHuman`。当前 `checkpoint.rs` 已改为保留每个文件的 checkpoint 历史；当 AI checkpoint 发现“最新一条 previous checkpoint 是最近的 IDE `KnownHuman` 且文件内容相同”时，会回退到它前一条状态（没有前一条时回退到 HEAD / INITIAL 基线）再计算 diff，而不是继续拿这条 `KnownHuman` 当比较基线。这样同内容的 AI checkpoint 仍能重新写出 AI 归因，不会再被 `KnownHuman` 抢占。实际验证中，已在 `op-return-exchange` 的临时副本里用 `known_human -> github-copilot after_edit -> commit -> stats/show/blame` 复现并确认：在 `GIT_AI_ASYNC_MODE=false` 的同步链路下，提交后的 `stats` 为 `ai_additions=10`、`show` 里的 prompt hash 归属 `github-copilot`、`blame` 也恢复为 `github-copilot`。

> **文档维护约定（2026-05-04）**：从这次开始，后续凡是与本文相关的实际代码改动，包括安装链路、上传链路、误归因修复、诊断日志和验证结论，都必须同步补充到本文顶部“实施补充”、Phase 清单以及后文“本次代码级修改”摘要中，避免文档与实现状态再次脱节。

> **实施补充（2026-05-04，debug 日志强化）**：`debug.jsonl` 已从“设置 `GIT_AI_DEBUG` 后才开启”改为默认开启，关闭时显式设置 `GIT_AI_DEBUG=false/0/off/no`；同时新增人类可读 `timestamp` 字段，并在日志超过 2GB 时尽力保留最近约 512MB 后继续写入，避免无限占用用户磁盘。为了不污染正常 commit 输出，JSONL 文件日志和 stderr 调试输出已经拆开，stderr 需要单独设置 `GIT_AI_DEBUG_STDERR`。上传链路补齐了逐步诊断事件：post-commit 主流程、stats 计算/跳过、payload 构建、URL/user-id/API-key 解析、HTTP 请求序列化、发送、响应、非 2xx、成功/失败都会写入结构化日志，且不记录代码内容、prompt 正文或 API key 明文。另新增中文说明文档 `docs/design-doc/git-ai环境变量与配置说明.md`，集中说明所有常用环境变量、查看方式、设置方式和排查组合。

> **实施补充（2026-05-04，北京时间时间戳）**：根据实际排查使用习惯，`debug.jsonl` 的人类可读 `timestamp` 已从 UTC 切换为固定北京时间 `UTC+08:00` 输出，示例格式为 `2026-05-04T22:31:45.123+08:00`；`timestampMs` 继续保留 Unix epoch 毫秒值，避免机器排序、跨机比对和后续服务端分析受显示时区影响。

> **实施补充（2026-05-04，最近两次 commit 统计核对）**：对 `bef6f7df` 和 `d9d879dc` 的本地 note / `git-ai stats` / `git diff-tree --numstat` / `debug.jsonl` 联合排查后确认：这两次提交的 commit 级 `aiAdditions=237` 都是本地统计先算出来的，不是服务端字段映射把同一个值重复写到了两次提交。真正的文件级异常出在 `src/integration/upload_stats.rs` 的逐文件 payload 构造：旧实现直接读取 `git diff-tree --numstat` 默认输出，而 Git 在 `core.quotepath=true` 时会把非 ASCII 文件路径转成带引号和字节转义的形式，例如 `"docs/design-doc/git-ai\347...md"`。这样它与 authorship note 里的 UTF-8 `attestation.file_path` 无法精确匹配，就会把该文件的 `aiAdditions` 错记为 `0`、`unknownAdditions` 错记为全部新增。当前实现已改为显式使用 `git -c core.quotepath=false diff-tree --numstat ...`，并补回归测试锁定中文文件名场景。

> **实施补充（2026-05-04，空白新增行归并）**：在 `op-return-exchange` 的 `FastFifthTest.java` 现场中，`git diff` 明确是新增 7 行，但 note 只覆盖了 `31-36` 这 6 行，导致方法尾部那个新增空白分隔行被本地 `git-ai stats` 和上传 payload 同时记成 `unknownAdditions=1`。根因不在服务端字段映射，也不在 `git diff-tree --numstat`，而在本地统计和 `stats.files[]` 之前都只是把“新增行号”与 authorship note 的 `line_ranges` 直接求交；当 whitespace-only 新增行没有直接落进 attestation range 时，它就会掉到 `unknown`。当前实现已在 `src/authorship/stats.rs` 增加 whitespace-only 新增行的邻接归并逻辑：只要该空白新增行紧邻同一侧或两侧一致的 KnownHuman / AI 已归因新增块，就跟随相邻块一起计入 human / ai；`src/integration/upload_stats.rs` 也复用了同一套按行归并结果，保证本地 `git-ai stats` 和上传 payload 口径一致。

> **实施补充（2026-05-04，Windows LLVM 工具链固化）**：当前 Windows 机器上真正挡住 `cargo test` 的不是业务代码，而是 PATH 先命中了 `Rust stable GNU 1.95`（`host=x86_64-pc-windows-gnu`）和 `D:\MinGW\mingw64\bin` 下的 GCC 8.1 `sjlj` 运行时，导致 `_Unwind_Resume` / `_GCC_specific_handler` 链接失败。现已在工作区 `.vscode/settings.json` 中为新终端和 rust-analyzer 的 cargo/check 统一前置 `Rust stable LLVM 1.95` 与 LLVM-MinGW 路径；同一条 `cargo test debug_timestamp_uses_beijing_time --lib` 在该环境下已不再出现 GNU unwind 链接错误，并成功跑过 `diagnostics::tests::debug_timestamp_uses_beijing_time`。另外顺手修复了 `src/authorship/stats.rs` 中 4 个仍在调用旧 `accepted_lines_from_attestations(repo, commit_sha, ...)` 之前签名的测试用例，避免环境修好后又被旧测试编译错误挡住。

> **实施补充（2026-05-04，上传成功判定与服务端主键修复）**：对 `/api/public/upload/ai-stats` 的源码和只读数据库核查表明，`ai-cr-manage-service` 的 `git_ai_tool_stats.id` 仍是 `INT/Integer`，但全局 MyBatis-Plus 主键策略已经是 `ASSIGN_ID` 雪花 long；现网表中已经出现大量负数主键和接近 32 位边界的值，最终会触发 `Duplicate entry ... for key 'git_ai_tool_stats.PRIMARY'`。当前服务端代码已把 `GitAiToolStats.id` 改为 `Long`，并在 `appendToolStats(...)` 中显式写入 `IdWorker.getId()`；数据库列仍需手工改成 `BIGINT`，对应 SQL 已单独整理到 `docs/design-doc/git_ai_tool_stats_primary_key_fix_2026-05-04.sql`。同时，`git-ai` 的原生上传现在不再只按 HTTP 2xx 判定成功，而会继续校验服务端 JSON 响应体里的 `code/msg`；当服务端返回 HTTP 200 但 body `code != 200` 时，本地会记成上传失败并把错误详情写入 `debug.jsonl`，避免继续把业务失败误报成“上传成功”。

> **实施补充（2026-05-04，prompt 仅保留用户输入）**：继续梳理 prompt 落库与上传链路后确认，`promptText` 实际上一直只从 `Message::User` 提取；真正把 assistant 输出一并带进 note / CAS / 本地 SQLite / 上传 payload 的，是 `PromptRecord.messages` 和 `stats.prompts[].messages` 过去一直保留整段 transcript。当前实现已改为：`post_commit` 在进入 `notes` / `default` / `local` 三种存储分支之前，会统一把所有 `PromptRecord.messages` 过滤成仅保留用户输入；`upload_stats.rs` 在序列化 `messages` 字段时，也会再次只输出 `Message::User`，用于兼容旧 note 中遗留的 assistant/tool 消息。这样从本次起，持久化与远程上传都会只保留用户输入，不再保存或上传 AI 输出、thinking、plan 和 tool_use 内容。

> **实施补充（2026-05-04，AI follow-up checkpoint 归因缩减修复）**：继续结合 `debug.jsonl` 和 working log 排查后确认，`src/integration/upload_stats.rs` 这类“先有一轮大块 AI 生成、随后同文件再做一次小 follow-up 编辑”的误归因，不是 `stats`/上传口径算错，而是旧实现会在 `RepoStorage::prune_old_char_attributions()` 中清掉旧 checkpoint 的 `entry.attributions` 以节省空间，但 `checkpoint.rs` 重新选择较早 previous checkpoint 作为 AI 基线时，又直接把这个已经被清空的 `entry.attributions` 当成完整旧归因使用。结果就是 follow-up AI checkpoint 只能保住“本次小改的几行”，前一轮大块 AI 代码会被 `attribute_unattributed_ranges(...)` 补回 human，最终 note / stats 里只剩最后几行还是 AI。当前实现已改为：`build_previous_file_state_maps(...)` 在发现旧 checkpoint 的字符归因已被裁剪、但 `line_attributions` 和 blob 仍在时，会用当时的 blob 内容把 `line_attributions` 还原成可继续演算的字符归因，再交给后续 diff/transform 逻辑。这样 AI follow-up checkpoint 会在复用旧基线时保住前一轮的大块 AI 归因，不再缩成“只剩最后改动的几行”。

> **实施补充（2026-05-08，commit 触发 CLI 自更新）**：`git-ai` 现在不再只在 `push` 之后调度后台升级检查；`src/commands/git_handlers.rs` 已经在 `git commit` 成功后的 wrapper 收口点补上 `upgrade::maybe_schedule_background_update_check()`，并同时覆盖“daemon 已连接”和“daemon 不可用时直通 real git”两条路径。这里复用的仍是现有 `git-ai upgrade --background` 机制，所以它继续受 24 小时版本检查间隔、60 秒后台启动节流、`disable_version_checks` / `disable_auto_updates` 和 `update_channel` 配置约束，不会阻塞本次 commit。当前实现里，版本发现已经直接读取 `configured_github_repo()` 对应的 GitHub releases，安装器也优先从同一仓库目标 tag 的 `install.ps1` / `install.sh` release asset 获取；如果 release asset 缺失，再回退到该仓库的 raw `main` 安装脚本。因此，对未来版本来说，自动更新不再依赖 `Config::api_base_url()` 下 `/worker/releases` 的脚本模板。

> **实施补充（2026-05-09，Copilot prompt/model 丢失的双重根因与现网切换）**：本轮排查确认，看板里缺少 model / prompt 不是单点问题，而是至少有两段独立链路同时出错。第一段是代码层：merge main 后，GitHub Copilot 的 session/trace 形态 checkpoint 在 `VirtualAttributions` 落 note 时只写 `metadata.sessions`，没有同步 materialize `metadata.prompts`，导致 `upload-stats` 继续按 `prompts={}` 计算，`promptCount` / `promptText` / prompt 级 tool 统计都会是 0。当前实现已在 `src/authorship/virtual_attribution.rs` 补齐 `s_<session>::t_<trace>` prompt record 物化、prompt metrics lookup 和回归测试，`cargo test record_checkpoint_agent_metadata_for_session_format_creates_prompt_and_session --lib`、`cargo test calculate_and_update_prompt_metrics_supports_session_trace_prompt_ids --lib`、`cargo test github_copilot --lib` 已通过。与 model 丢失直接相关的同一轮 merge 回归还有一条：Copilot native hook 路径把 `transcript_path` 和 `chat_session_path` 合并成同一个 metadata 值，导致本来只存在于 session JSON 的 `inputState.selectedModel.identifier` 被 event-stream path 覆盖；同时 `src/transcripts/model_extraction.rs` 只保留了对 `data.modelId` / `data.modelID` 的扁平读取，丢掉了旧 Copilot reader 中对 `selectedModel.identifier` 和递归 model hint 的提取能力。当前实现已恢复这两条路径：native hook 保留真实 `chat_session_path` 并优先用它提 model，`model_extraction.rs` 重新支持 `selectedModel.identifier`、递归 Copilot model hint 以及 session/event-stream 两种格式的回归测试。第二段是运行时层：仅修复源码还不够。Windows 现场机器上 `~/.copilot/hooks/git-ai.json` 仍然硬编码旧安装版 `C:\Users\admin\.git-ai\bin\git-ai.exe`，即使新版已经通过 replacement daemon runtime 接管 trace2 pipe，下一次 Copilot native hook 仍会把旧 binary 再次拉起，真实 commit 继续产出 `promptCount=0` 的 note。现网恢复步骤是：先用目标新版 binary 执行 `git-ai bg restart --hard` 激活 replacement runtime，再把 `~/.copilot/hooks/git-ai.json` 的 `checkpoint github-copilot --hook-input stdin` 命令切到同名新版 binary（本次验证使用 `C:\Users\admin\.git-ai\patched\git-ai.exe`），确保 checkpoint 与 post_commit 进入同一套新版 runtime。外部真实验证在 `D:\rj-op\op-return-exchange` 的 `AITest.java` 上完成：旧 hook 路径对应的提交 `b76dd87554915175a40d5e177c4f873fa7220807` 在 `debug.jsonl` 中仍显示 `processId=36376`、`promptCount=0`；切换 hook JSON 并重启 patched daemon 后，提交 `17fa3b123941bb8bf95e50da7a6c6adc93932fd3` 已生成带 `prompts/messages` 的 authorship note，`debug.jsonl` 中 `post_commit_authorship_log_built` / `upload_stats_payload_build_succeeded` 均显示 `processId=11892`、`promptCount=1`、`promptsWithText=1`，服务端返回 `code=200`。需要单独说明的是，如果某次 VS Code event-stream 和 session JSON 本身都没有任何 `model` / `modelId` / `modelID` / `selectedModel.identifier`，最终 `agent_id.model` 仍然只能是 `unknown`；git-ai 只能恢复已有字段，不能凭空推断模型名。

> **实施补充（2026-05-10，Windows 真机验证与 GitHub latest 修正要求）**：本轮又在 Windows 真机对 `op-return-exchange` 做了真实 commit 验证，确认“提示词又没了”的最终根因不是 `prompt_utils.rs` 单点解析失效，而是安装目录下的新 `git-ai.exe` / `git.exe` 已经替换，但常驻 `git-ai.exe bg run` daemon 仍在执行旧内存镜像，导致 post-commit 继续按旧逻辑生成空 prompt note。现网恢复步骤是：同步安装版 `git-ai.exe` 与 `git.exe` 后，必须再执行一次 `git-ai bg restart`，确保 checkpoint、post_commit 和上传链路都进入同一套新 runtime；本次最终验证 commit `7febcecf61f151f5e73eb61164088fc27c56cb72` 的 ai note 已不再出现 `"messages": []`，并能检测到 `9` 条 user message。与此同时，外部安装 / 更新链路还暴露出第二个独立问题：`https://github.com/rj-gaoang/git-ai/releases/latest/download/git-ai-windows-x64.exe` 当时仍跳转到 `v2.1.9`，说明 GitHub `latest` 指向并没有自动跟上目标版本；更早一次核查里，名义上的 `v2.1.11` Windows asset 还曾回报 `2.1.10`。这意味着“源码已修好”和“外部 latest 能拿到正确 release asset”必须分开闭环。发布侧修正的最低要求是：先在目标 commit 上确认 `Cargo.toml` / 构建产物版本已经是 `2.1.11`，再通过 `.github/workflows/release.yml` 以 `dry_run=false` + `release_production=true` 发布稳定版，使 workflow 产出 `tag_name=v2.1.11`、`prerelease=false`、`make_latest=true`；如果线上已经存在错误的 `v2.1.11` release / tag / asset，必须先删除错误 release 或替换错误 asset，再从正确 commit 重跑 production release。发布完成后，还要再次核对 `/releases/latest/download/...` 的 302 跳转目标以及下载后二进制的 `--version`，确认 latest 与资产版本完全一致。

> **实施补充（2026-05-14，commit 后自动更新踩坑总结）**：对 2026-05-10 之后这轮“`git commit` 成功即后台自更新”的实现、真机验证和现场回滚复盘后，可以把踩过的坑明确收敛成 7 类。1）**触发点补上不等于真实生效 binary 已切换**：即使 commit 路径已经调度 `upgrade --background`，Windows 上 `~/.copilot/hooks/git-ai.json` 仍可能继续拉旧安装版 `git-ai.exe`。2）**安装目录文件已替换，不等于常驻 daemon 已更新**：若不显式 `git-ai bg restart` 或自动完成 runtime handoff，post-commit 仍会跑旧内存镜像。3）**restart --hard 还要同时刷新 trace2 指向**：否则 wrapper / post-commit 仍可能把事件写到旧 pipe。4）**源码修好不等于 latest release 已修好**：GitHub `latest` 指向和 release asset 版本可能继续落后或错配。5）**Windows 安装器在 `Checksum verified` 之后还会先等待 `upload_activity.lock`**，如果没有显式日志，很容易被误判成“下载卡死”。6）**安装器只靠 `Win32_Process` 枚举并吞掉 `Stop-Process` 失败**，会把 ghost PID 或已退出进程对象当成 lingering process，现场只能看到重复 PID，看不到真正 kill 失败原因。7）**hook 链路如果没有写完并关闭 stdin，会留下卡死的 `checkpoint ... --hook-input stdin` 进程**，直接阻塞安装目录 `git-ai.exe` 替换；即使 replacement runtime 已能绕过旧锁继续服务，运行时拷贝若没有及时回收，仍会继续放大 `internal` 目录膨胀和新旧 runtime 混跑的问题。

> **实施补充（2026-05-26，本地上传状态与自动补传）**：为解决上传失败后缺少自动回补的问题，`git-ai` 已在仓库本地 `.git/ai/upload_stats_status.json` 增加 note 上传状态索引。每条 authorship note 对应的 commit 会记录 `uploadStatus=not_uploaded/succeeded/failed`、note blob id、最近尝试时间、成功/失败时间、状态码和错误摘要。自动上传和 `git-ai upload-stats` 都会在发送当前 commit 前扫描当前分支可达的 `refs/notes/ai`：本地状态已登记为 `failed` 或 `not_uploaded` 的 note，会按“失败优先、未上传其次”的顺序和当前 commit 放入同一个 `commits[]` batch 补传；默认每次最多补带 25 条，可通过 `GIT_AI_UPLOAD_BACKLOG_LIMIT` 调整。首次发现的历史 note 会先登记为 `not_uploaded`，后续上传逐步回补，避免老仓库第一次上传时一次性重算全部历史 payload。上传成功后批量标记为 `succeeded`，HTTP / 后端业务失败后批量标记为 `failed`，下次继续重试。如果某条 note 的 blob id 变化，即使之前成功过，也会自动回到 `not_uploaded`，避免改写 note 后被误认为已经上传。

> **实施补充（2026-05-27，Windows 内网安装二进制来源固定）**：结合黄芳机器 `debug(8).jsonl`、`service.log` 和 `install-v2.2.22-20260526113329.log` 排查后确认，2026-05-23 19:47:14 之后本地 debug 日志只剩 Copilot hook / checkpoint 事件，没有新的 `post_commit_*` / `upload_stats_*`，说明后续统计未上传发生在 git-ai post-commit/proxy 之前，不是 HTTP 上传失败；同时 2.2.22 更新日志显示安装前 `git-ai.exe` 被多个 `bg run` / `checkpoint --hook-input stdin` 进程占用，旧 service wrapper 未做预清理且上游 `install.ps1` 仍打印 `Downloading git-ai (repo: rj-gaoang/git-ai, release: v2.2.22)`，证明二进制来源仍可能走 GitHub release asset。当前 `git-ai/install.ps1` 已新增内网 binary base 解析：当 `GIT_AI_BINARY_BASE_URL` 或 `RUIJIE_AI_GIT_AI_BASE_URL` 存在，或能从非 GitHub 的 `GIT_AI_INSTALLER_URL` 推导出 base URL 时，Windows 安装会优先下载同目录的 `git-ai-windows-x64.exe` / `git-ai-windows-arm64.exe`，并在该内网来源失败时直接报错，不再回退 GitHub。配合 `windows-update-service` wrapper 已有的 `GIT_AI_RELEASE_TAG=<target_tag>`、`pre_install_cleanup` 和 PATH 诊断，内网更新链路需要发布 `ruijie-ai/install.ps1` 与对应平台二进制到同一目录，再重装/更新 service 包。

> **实施补充（2026-05-28，Windows 安装器重命名替换策略）**：黄芳机器升级链路继续暴露出“不能停止 C 进程、但旧 `git-ai.exe` 被占用”的现实约束。当前 `git-ai/install.ps1` 在版本化目录 + stable launcher 基础上补入“方案二”重命名替换策略：`Install-LauncherBinary` 遇到目标 exe busy 时，会先把旧文件 `Move-Item` 到同目录 `.old.<timestamp>.<pid>` 名称，再复制新 launcher 到原路径；旧文件随后优先立即删除，若仍被进程持有，则通过 `MoveFileEx(..., MOVEFILE_DELAY_UNTIL_REBOOT)` 登记为重启后删除。若 Windows 因句柄未开启 delete sharing 导致重命名失败，安装器不会杀 C 进程；必需 launcher 会明确失败，非必需兼容 launcher 只 warning 并返回失败，避免旧的 busy 文件被静默当成更新成功。

> **实施补充（2026-05-31，自动上传 activity lock 超时兜底）**：黄芳机器 `debug(11).jsonl` 已经能看到 `post_commit_*`、`upload_stats_ready` 和 `upload_stats_started`，说明安装 / post-commit 入口已经恢复；真正阻断上传的是本地 `C:\Users\admin\.git-ai\internal\upload_activity.lock` 在 5 秒内不可用，且失败前没有任何 `upload_stats_http_request_ready` / `upload_stats_http_response_received`。当前 `git-ai` 自动上传路径调整为：后台 / inline 自动上传等待 activity lock 超时后，额外写入 `upload_stats_activity_lock_bypassed`，并继续按 best-effort 发送 HTTP，避免陈旧锁或旧 runtime 残留句柄让统计结果永久停在本地锁之前；手动 `git-ai upload-stats` 仍保持 30 秒严格等待，不绕过锁，避免用户主动补传时并发改写本地上传状态。

> **实施补充（2026-06-01，Windows daemon blocked startup 不再切 replacement runtime）**：继续结合黄芳机器 `install-v2.2.22-20260529082319.log` 与 `debug(11).jsonl` 排查后确认，`upload_stats_activity_lock_timeout` 背后不只是“锁等待不够久”，而是当时机器上已经同时存在 52 个 `git-ai.exe` 进程，多数为 `bg run`。真正放大问题的控制点在 daemon 恢复逻辑：旧实现遇到 `daemon.lock` 被占用、control/trace socket 不可用且 `taskkill` 无法回收旧 PID 时，会继续激活 replacement runtime，导致多套 daemon runtime 并存，再一起争用全局 `upload_activity.lock`。当前 `git-ai/src/commands/daemon.rs` 已改为：若 `hard_kill_daemon_pid(...)` 失败，则直接保留 blocked startup 错误，拒绝激活 replacement runtime，从根上阻断多 daemon 扩散。对应回归测试 `daemon_start_refuses_replacement_runtime_when_blocked_pid_cannot_be_killed`、`checkpoint_fails_hard_when_daemon_startup_is_blocked`、`daemon_restart_when_not_running_starts_fresh` 已通过。

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
| **原生自动上传入口** | `src/authorship/post_commit.rs` + `src/integration/upload_stats.rs` | 在 authorship note 写入成功且 stats 可用时，直接组装与脚本一致的 payload 并后台上传 | 适合 commit 后即时上报 |
| **本地存储** | `refs/notes/ai`（Git Notes） | 每次 commit 自动生成的 AI 归因日志 | 这是所有统计的数据源 |
| **Authorship Note 直接解析** | `git notes --ref=ai show <sha>` | 输出该 commit 的原始 attestation（逐文件、逐行范围的 AI/人工归因）+ JSON 元数据（prompt 的 tool/model 信息） | 上传脚本解析它获取**逐文件级**的 AI 归因明细（`Get-CommitAiFileStats`） |
| **X-USER-ID 解析** | `src/integration/ide_mcp.rs` | 从环境变量、仓库及父级工作区 `.mcp.json` / `.vscode/mcp.json`、VS Code / IDEA MCP 配置中读取 `X-USER-ID` | 原生上传和脚本上传都复用同一套身份来源 |
| **推送机制** | `push_authorship_notes()`（`src/git/sync_authorship.rs`） | git push 时自动把 notes 推到远端 | 已有，无需改动 |

**关键发现（影响设计决策）：**
- `git-ai stats <sha> --json` 仍然足够支撑 Speckit 的批量上传 / Code Review 上传；但如果要求“commit 生成统计结果时立即上报”，就必须补 `git-ai` 的 Rust 原生上传路径
- 最新安装脚本已经会自动执行 `git-ai install-hooks`；`git-ai git-hooks ensure` 这条旧路径在当前代码里已经 sunset，不能再作为主方案依赖
- 当前 `git-ai config` 只支持固定顶层键以及 `feature_flags.*`、`git_ai_hooks.*` 这两类嵌套键，**不支持** `report_to_remote.*` 这一类自定义上传配置；远程 endpoint / api_key 需要通过环境变量或 Speckit 自己的脚本配置解决
- 对于“某个 commit 有没有 AI authorship note”，不能依赖 `git-ai stats` 是否为空来判断；当前可靠做法是直接查询 `git notes --ref=ai list <sha>`
- 最新 `git-ai` 已内建 `api_base_url` / `api_key` / `personal-dashboard` 这条官方后端路径；如果团队可以接受 Git AI Enterprise 或自托管后端，应优先评估原生路径，本文 3.3-3.5 仅针对“必须对接自有 API”的情况
- commit 时已经在本地写好 authorship note 了，我们只需要在需要的时候"读取 + 上传"
- 当前已新增 `feature_flags.auto_upload_ai_stats`，默认开启；如需显式覆盖，请设置 `GIT_AI_AUTO_UPLOAD_AI_STATS=true/false`，其中 `1` 不会被识别为 `true`
- **逐文件统计使用 commit-local 语义**：直接解析 `git notes --ref=ai show <sha>` 的 attestation 段（非缩进行=文件路径，缩进行=`<id> <range>` 归因条目，`h_*` 前缀=人工，其他=AI prompt hash），结合 `git diff-tree --numstat` 获取每个文件的新增/删除行数，无需调用 `git-ai diff`（后者是 provenance-traced，会跨 commit 追溯，不适合 commit-local 场景）

---

## 二、需求 1 详细实施：安装 Speckit 时自动安装 git-ai

### 2.1 我们要达到什么效果？

**目标：** 新成员 clone 项目仓库后，执行 Speckit 初始化流程时，git-ai 被自动安装；后续执行 Spec Kit 更新流程时，git-ai 也会被同步更新到目标最新版本，并完成 `install-hooks` 初始化，整个过程无需成员额外操作。

**为什么不让成员自己装？**  
因为实际情况是：你发一个安装文档给 10 个人，最终只有 3 个人会照做。自动化安装才能保证团队覆盖率。

### 2.2 技术方案选择——为什么这样做？

Speckit 没有标准的"安装完成后自动执行某个脚本"的机制（没有 postInit hook）。所以我们考虑了 3 种方案：

| 方案 | 做法 | 优点 | 缺点 | 结论 |
|------|------|------|------|------|
| A. 直接修改 `specify init` 源码 | 在 `specify_cli/__init__.py` 的 `init()` 末尾自动执行 `.specify/scripts/<script-type>/post-init` | 真正覆盖 `specify init .` 和 `specify init --here --force`，不改变用户命令习惯 | 需要改 Spec Kit Python 源码 | ✅ **采用**（主方案） |
| B. 修改 Agent prompt | 在 `speckit.specify.agent.md` 里加一步"先执行 post-init.ps1" | 改动小，适合补充 agent 流程体验 | 只在用户通过 Agent 走流程时触发，不覆盖 CLI 初始化 | ⚪ **可选兜底** |
| C. 改 check-prerequisites | 在 `check-prerequisites.ps1` 开头检测 git-ai | 对存量项目友好，能提醒旧仓库补装 | 不是安装时触发，只是检查提醒 | ⚪ **可选兜底** |

**最终决策：A 为主，B/C 只作为补充。**

原因很简单：你的目标不是“第一次使用 `/speckit.specify` 时装 git-ai”，而是“当用户执行 `specify init` 或重新执行 `specify init --here --force` 更新项目时，就顺手把 git-ai 装好”。

只有直接修改 `specify_cli/__init__.py` 的 `init()` 流程，才能精准覆盖这个目标。Agent prompt 和 `check-prerequisites.ps1` 最多只能覆盖“初始化后的后续使用阶段”，不能替代 CLI 初始化入口本身。

这里还要补一个关键修正：旧设计里 `post-init.ps1` / `post-init.sh` 的默认语义其实是“已安装则跳过，只有显式 `-Force` / `--force` 才重装”，而 `specify_cli.__init__.py` 之前调用 `run_post_init_script(...)` 时并没有传这个参数。所以旧方案实际上只实现了“缺失时安装”，没有实现“安装或更新 Spec Kit 时同步升级 git-ai”。要满足新要求，`specify init` 必须始终以 force-update 语义执行 post-init。

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

1. **默认初始化路径**：`specify init` 优先走 wheel 里的 `core_pack`，只有本地 bundle 不可用时才回退
2. **强制离线路径**：`specify init --offline` 只走 wheel 里的 `core_pack`，不允许网络回退
3. **显式在线回退**：设置 `SPECIFY_INIT_USE_GITHUB_RELEASE=true` 时，可以强制回退到 GitHub release ZIP
4. **离线打包来源**：`core_pack/scripts/powershell` 和 `core_pack/scripts/bash` 都是从仓库根下的 `scripts/` 目录打包进去的

所以，**真正的模板源文件是 `spec-kit/scripts/powershell/*.ps1` 和 `spec-kit/scripts/bash/*.sh`**。

这意味着：

- 你如果只改 `spec-kit/.specify/scripts/powershell/post-init.ps1`，仓库里看起来有文件了，但 `specify init` 生成新项目时不一定会带上这份改动
- 你如果只改 `scripts/`，通常已经足够影响模板生成；但为了让仓库自己的验证目录和开发态体验保持一致，仍然应该同步 `.specify/` 和 `test-verify/.specify/` 的副本
- `pyproject.toml` 在当前实现里**不用改**，因为 `scripts/powershell` 和 `scripts/bash` 早就已经被 `force-include` 到 wheel 的 `core_pack` 里了

### 2.3 具体要做的事情（逐步操作）

#### 2.3.0 建议按这个顺序动手，不容易漏

如果你要让别人“照着做就能落地”，推荐严格按下面顺序执行：

1. 先在 `spec-kit/scripts/powershell/` 和 `spec-kit/scripts/bash/` 新增真正的模板源脚本
2. 再同步仓库里的 `.specify/` 和 `test-verify/.specify/` 副本
3. 然后修改 `spec-kit/src/specify_cli/__init__.py`，把 post-init 执行点接进 `init()` 主流程
4. 最后再补 `check-prerequisites.ps1` 的兜底提示

为什么这个顺序更稳：

- 先有脚本源，再接入口，避免 `init()` 已经开始调用 post-init，但模板里其实还没有脚本
- 先保证 `scripts/` 源文件正确，再同步副本，避免后面维护时多个文件反向漂移
- 最后再做兜底检查，是因为它不影响主流程，只是辅助提醒

#### 第 1 步：创建 `post-init` 脚本（PowerShell 为主，bash 同步补齐）

**要做什么：**

- 在 `.specify/scripts/powershell/` 目录下新建 `post-init.ps1`
- 如果要支持 `specify init --script sh`，同时在 `.specify/scripts/bash/` 下新建 `post-init.sh`

**为什么：** 这个脚本封装了"检测 → 安装 / 更新 → 刷新 install-hooks"的完整逻辑。Spec Kit CLI 只需要负责在初始化结束后调用它，而不需要把 git-ai 安装细节硬编码进 Python 源码。这样做能保持职责清晰：

- `specify init` 负责“何时调用 post-init”
- `post-init.ps1` 负责“git-ai 到底怎么安装、怎么刷新 install-hooks”

另外，当前这个定制版本已经明确**不依赖目标项目仓库根目录里恰好存在 `install.ps1/install.sh`**。它默认直接从 `rj-gaoang/git-ai` 的 `main` 分支源码地址下载安装器，也就是 `https://raw.githubusercontent.com/rj-gaoang/git-ai/main/install.ps1` / `install.sh`。这样可以直接绕过历史 release 资产里滞后的安装脚本，同时 Spec Kit 仍然会自动补上 `GIT_AI_GITHUB_REPO=rj-gaoang/git-ai` 和 `GIT_AI_RELEASE_TAG=latest`，确保最终安装的二进制继续来自团队仓库的 latest release；如果后续要临时切到其他来源，仍然可以用 `GIT_AI_INSTALLER_URL`、`GIT_AI_GITHUB_REPO` 和 `GIT_AI_RELEASE_TAG` 覆盖。

**文件路径：**

- PowerShell: `.specify/scripts/powershell/post-init.ps1`
- Bash: `.specify/scripts/bash/post-init.sh`

**如果你当前是在 Spec Kit 源码仓库里改代码，这里要转换成真实落点：**

- 先改：`spec-kit/scripts/powershell/post-init.ps1`
- 再改：`spec-kit/scripts/bash/post-init.sh`
- 再同步：`spec-kit/.specify/scripts/powershell/post-init.ps1`
- 再同步：`spec-kit/test-verify/.specify/scripts/powershell/post-init.ps1`

也就是说，文档里写的 `.specify/scripts/...` 是“生成后的项目里会出现的路径”；而你在 Spec Kit upstream 仓库里真正要编辑的主文件，是 `scripts/...` 目录下那两份源脚本。

**已经验证过的 PowerShell 原型代码（可直接作为 upstream 实现基线）：**

```powershell
#!/usr/bin/env pwsh

[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$Skip
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot/common.ps1"

$GitAiDefaultGithubRepo = 'rj-gaoang/git-ai'
$GitAiDefaultInstallerRef = 'main'
$GitAiDefaultReleaseTag = 'latest'
$GitAiInstallScriptUrl = if ($env:GIT_AI_INSTALLER_URL) {
    $env:GIT_AI_INSTALLER_URL
} else {
    "https://raw.githubusercontent.com/$GitAiDefaultGithubRepo/$GitAiDefaultInstallerRef/install.ps1"
}
if ([string]::IsNullOrWhiteSpace($env:GIT_AI_GITHUB_REPO)) {
    $env:GIT_AI_GITHUB_REPO = $GitAiDefaultGithubRepo
}
if ([string]::IsNullOrWhiteSpace($env:GIT_AI_RELEASE_TAG)) {
    $env:GIT_AI_RELEASE_TAG = $GitAiDefaultReleaseTag
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
if ($existingCommand -and -not $Force) {
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

**注意：** 上面的 `post-init.ps1` 仍然保留 `-Force` 参数，作为可复用的手工入口；但产品级语义已经不能再依赖“用户自己记得加 `-Force`”。真正需要修改的是 `specify_cli.__init__.py` 对 post-init 的调用方式：`specify init` 和 `specify init --here --force` 都要以 `-Force` / `--force` 调用 post-init，这样才能满足“安装或更新 Spec Kit 时同步升级 git-ai”的要求。

**补充说明：** 当前真实实现里，bash 路径的模板源文件是 `spec-kit/scripts/bash/post-init.sh`；仓库内 `.specify/` 自举目录目前仍然只维护 PowerShell 脚本。无论 PowerShell 还是 bash，最终目标行为都一致：都是“检测已有安装 → 调官方安装器 → 刷新 install-hooks → 失败只 warning”。

**这里再强调一次职责边界：**

- `post-init.ps1` / `post-init.sh` 只负责 git-ai 的安装与 hooks 配置
- `specify_cli/__init__.py` 只负责“什么时候执行 post-init”
- 远程统计 endpoint / api_key 不属于当前默认安装逻辑，保持为后续可选增强

**验证方法：** 创建完脚本后，在项目根目录执行以下命令验证：

```powershell
# 测试脚本是否能正常运行
.\.specify\scripts\powershell\post-init.ps1

# 预期输出（已安装 git-ai 的情况）：
# [speckit/post-init] git-ai 已安装: git-ai x.x.x
# [speckit/post-init] git-ai 已安装，跳过安装步骤
# [speckit/post-init] 在仓库中配置 git-ai hooks: C:\Users\xxx\project
# [speckit/post-init] ✓ git-ai hooks 配置成功
# [speckit/post-init] ✓ git-ai 集成配置完成！
```

---

#### 第 2 步：修改 `specify_cli/__init__.py`，让 `specify init` 自动执行 post-init

**要做什么：** 在 Speckit upstream 源码的 `src/specify_cli/__init__.py` 中，给 `init()` 函数增加一个通用的 post-init 执行步骤。

**为什么必须改这里：**

- `specify init . --ai copilot` 和 `specify init --here --force --ai copilot` 最终都走这个 `init()` 函数
- 只改这里一处，就能同时覆盖“首次初始化”和“后续更新模板”两种场景
- 这样用户不需要改命令，不需要额外记一个 `setup-dev.ps1` 包装脚本

**具体怎么改：**

1. 在 `ensure_constitution_from_template()` 这类初始化 helper 附近，新增或扩展两个函数：
    - `_get_post_init_command(project_path, script_type, force_update: bool)`：根据脚本类型解析 `.specify/scripts/powershell/post-init.ps1` 或 `.specify/scripts/bash/post-init.sh`，并在 `force_update=true` 时自动附加 `-Force` / `--force`
    - `run_post_init_script(..., force_update: bool)`：真正执行脚本，失败只记录 warning，不中断 `specify init`
2. 在 `init()` 中，放在这段逻辑之后执行：
    - `save_init_options(...)`
    - preset 安装逻辑（如果有 `--preset`）
3. 执行顺序必须是：
    - 模板/脚本已经落盘
    - preset 已经安装完成（因为 preset 可能提供或覆盖 post-init 脚本）
    - 然后才执行 post-init，并且 CLI 路径默认使用 force-update 语义

**推荐插入点：** 放在 `# Install preset if specified` 这段逻辑之后、`tracker.complete("final", "project ready")` 之前。

**真实修改时，建议按下面这 4 个检查点去改：**

1. 在 helper 区新增 `_get_post_init_command(...)`
2. 在 helper 区新增 `run_post_init_script(...)`
3. 在 tracker 初始化时加入 `post-init` 这个步骤
4. 在 `init()` 主流程里，把 `run_post_init_script(project_path, selected_script, force_update=True, ...)` 接到 preset 安装之后、final 完成之前

**核心原则：**

- Python 代码只负责“找脚本并执行”
- 不在 Python 里硬编码 git-ai 安装细节
- post-init 执行失败只 warning，不让 `specify init` 整体失败
- 是否“安装缺失版本”还是“升级到目标最新版本”的决策，不再交给用户记忆命令参数，而是由 CLI 在安装 / 更新路径里统一传 `force_update=True`

**这样用户最终得到的体验：**

```bash
specify init . --ai copilot
```

执行完后，Spec Kit 会自动继续执行：

```powershell
.specify/scripts/powershell/post-init.ps1
```

用户不需要再手动补一步。

**验证方法：** 修改后，在一个新项目目录里执行：

```bash
specify init . --ai copilot
```

预期结果：

1. `Project ready.` 正常出现
2. 随后看到 post-init 的输出，如：
    - `[speckit/post-init] git-ai 未安装`
    - `[speckit/post-init] ✓ git-ai hooks 配置成功`
3. 最终执行 `git-ai --version` 有输出

---

#### 第 2.5 步：做完以后怎么验收，才算真的落地

很多方案文档只写“改完就行”，但别人真正照着做时，最怕的是不知道什么时候算完成。下面这套验收顺序，建议直接照着走。

**第一层：看文件有没有落对地方**

至少要能看到这些文件存在：

- `spec-kit/scripts/powershell/post-init.ps1`
- `spec-kit/scripts/bash/post-init.sh`
- `spec-kit/.specify/scripts/powershell/post-init.ps1`
- `spec-kit/test-verify/.specify/scripts/powershell/post-init.ps1`
- `spec-kit/src/specify_cli/__init__.py` 已经包含 post-init 相关 helper 和调用点

**第二层：做静态语法检查**

建议至少跑这几个检查：

```powershell
# Python 入口语法检查
python -m py_compile spec-kit/src/specify_cli/__init__.py

# PowerShell 语法解析检查（示例）
[System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path "spec-kit/scripts/powershell/post-init.ps1"),
    [ref]$null,
    [ref]$null
)

# Bash 语法检查
bash -n spec-kit/scripts/bash/post-init.sh
```

**第三层：做一次真实初始化测试**

在一个干净目录里执行：

```bash
specify init . --ai copilot
```

如果你要验证 bash 路径，再执行：

```bash
specify init . --ai copilot --script sh
```

**第四层：看最终效果而不是只看命令退出码**

至少要确认下面 4 件事：

1. `Project ready.` 正常出现
2. init 结束后出现 post-init 的输出日志
3. 生成出来的项目里真的有 `.specify/scripts/powershell/post-init.ps1` 或 `.specify/scripts/bash/post-init.sh`
4. 安装成功场景下，`git-ai --version` 有输出；如果 shell PATH 未刷新，也至少会看到手动补跑 `git-ai install-hooks` 的 warning 提示

只要这 4 层都通过，才说明“方案真的落地了”，不是只改了文档或者只改了仓库里的某个副本。

---

#### 第 3 步（可选兜底）：在 check-prerequisites 中加入 git-ai 检测

**要做什么：** 在 `.specify/scripts/powershell/check-prerequisites.ps1` 的前置检查中增加 git-ai 安装状态检测。

**为什么：** 这一步不再是主触发机制，而是给**存量项目**做兜底。比如：

- 某个项目是在“还没改 upstream `init()`”之前初始化的
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

### 3.1 上传策略更新：从纯脚本触发扩展为“双轨制”

这份文档最初的判断是：`git-ai` 在每次 commit 时只负责把 authorship note 写到本地，上传放在 Speckit 脚本层做即可。这个判断对“批量回补”和“Code Review 附带上传”依然成立，但它**不能满足**当前新增的要求：

> 在 `git-ai` 生成统计结果的时候，就直接把同样结构的数据上传到远程。

因此，当前设计已经从“只有 Speckit 脚本上传”扩展为“双轨制 / 三入口”：

| 路径 | 触发点 | 实现位置 | 默认状态 | 适用场景 |
|------|--------|---------|---------|---------|
| **A. git-ai 原生自动上传** | `git commit` 后 `post_commit` 成功写 note 且 stats 可用时 | `git-ai/src/authorship/post_commit.rs` + `git-ai/src/integration/upload_stats.rs` | 默认开启；如需覆盖，使用 `GIT_AI_AUTO_UPLOAD_AI_STATS=true/false` | 需要 commit 后即时上报 |
| **B. Speckit 手动批量上传** | 开发者手动执行 | `.specify/scripts/powershell/upload-ai-stats.ps1` | 按需执行 | 日期范围、指定 commit、历史回补 |
| **C. Code Review 自动上传** | 发起 `/speckit.code-review` 时 | Code Review Agent + 报告模板 | 跟随审查流程 | 审查兜底、报告展示、补齐遗漏 |

**三条路径不是互斥关系，而是职责分层：**
1. **A 路径解决实时性**：当前 commit 一落盘就可尝试上报，最接近“即时统计”。
2. **B 路径解决回补与重放**：历史 commit、日期范围、指定 commit 仍然更适合脚本批处理。
3. **C 路径解决治理闭环**：即便开发者忘了手动上传，Code Review 仍可以做最后一次补传和展示。

### 3.2 整体数据流（一张图看懂全链路）

```
  开发者写代码（用 AI 工具辅助）
      │
      ▼
  git commit（本地触发 git-ai hooks）
      │
      ▼
┌────────────────────────────────────────┐
│ git-ai post_commit（本地核心链路）      │
│                                        │
│ 1. 分析 diff，生成 authorship log       │
│ 2. 写入 Git Note（refs/notes/ai）       │
│ 3. 计算 CommitStats（若 merge / 过大提交 │
│    则可能跳过 stats）                   │
│ 4. 若 auto_upload_ai_stats 开启（默认开启，│
│    可用 true/false 覆盖）且 stats 可用： │
│    发起一次单 commit 上传（best-effort） │
└──────────┬─────────────────────────────┘
           │
           │ 同一份本地数据还可以被其他路径复用：
           │
    ┌────────┴─────────────────────────┐
    │                                  │
    ▼                                  ▼
 ┌───────────────────┐     ┌───────────────────────┐
 │ 路径 B：手动批量上传 │     │ 路径 C：Code Review    │
 │                    │     │ 自动上传 / 展示         │
 │ 开发者执行          │     │ leader 发起 code review│
 │ upload-ai-stats.ps1│     │ Agent 步骤 8.3/8.4     │
 │                    │     │                       │
 │ 数据来源：          │     │ 数据来源：              │
 │ git-ai stats +     │     │ git-ai stats +         │
 │ authorship note    │     │ authorship note        │
 └────────┬───────────┘     └──────────┬────────────┘
        │                            │
        └──────────────┬─────────────┘
                    ▼
 ┌────────────────────────────────────────────────────┐
 │ 远程 API                                            │
 │ - schema 与 upload-ai-stats.ps1 一致               │
│ - 服务端按 commitSha 做幂等去重                     │
 │ - native 路径当前写入 source="auto"               │
 │ - 脚本 / review 路径分别使用 manual / codeReview    │
 └────────────────────────────────────────────────────┘
```

#### 3.2.1 当前已落地的 git-ai 源码改动

| 文件 | 当前改动 | 作用 |
|------|---------|------|
| `git-ai/src/feature_flags.rs` | 新增 `auto_upload_ai_stats` | 默认开启原生自动上传；如需覆盖，使用 `GIT_AI_AUTO_UPLOAD_AI_STATS=true/false` |
| `git-ai/src/integration/mod.rs` | 导出 `upload_stats` 模块 | 把上传能力纳入 `integration` 命名空间 |
| `git-ai/src/integration/upload_stats.rs` | 新增 Rust 原生上传模块，并维护本地上传状态索引 | 复刻 `upload-ai-stats.ps1` 的 API schema、headers 和逐文件统计语义；同时把当前 commit 与已登记的失败 / 未上传 note 合并补传，默认每次最多补带 25 条积压记录 |
| `git-ai/src/authorship/post_commit.rs` | 在 note 写入和 stats 计算后调用 `maybe_upload_after_commit(...)` | 将原生上传接入 commit 主链路，但保持 best-effort |
| `git-ai/src/integration/ide_mcp.rs` | 复用现有 `resolve_x_user_id(...)` | 当 `GIT_AI_REPORT_REMOTE_USER_ID` 未设置时，从 MCP 配置读取 `X-USER-ID` |

#### 3.2.2 原生上传与脚本上传的对齐点

当前 Rust 原生实现不是重新发明一套接口，而是**对齐 Speckit 脚本已经验证过的同一份远程协议**：

1. **同一个 endpoint 约定**：优先读 `GIT_AI_REPORT_REMOTE_URL`；否则读 `GIT_AI_REPORT_REMOTE_ENDPOINT` + `GIT_AI_REPORT_REMOTE_PATH`；都没有时回退到默认地址 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`。
2. **同一组认证头**：`Content-Type: application/json`、可选 `Authorization: Bearer <api_key>`、可选 `X-USER-ID`。
3. **同一份 payload 主结构**：`repoUrl`、`projectName`、`branch`、`reviewDocumentId`、`authorshipSchemaVersion`、`clientContext`、`commits[]`。
4. **同一份 commit 级 stats 结构**：`humanAdditions`、`unknownAdditions`、`mixedAdditions`、`aiAdditions`、`aiAccepted`、`toolModelBreakdown[]` 等字段都保持 camelCase 兼容。
5. **同一份文件级统计语义**：继续使用 commit-local 方式，从 authorship note 的 attestation + `git diff-tree --numstat` 组合出 `stats.files[]`；默认排除 `target/` 这类构建产物目录，避免文件表被编译输出刷屏，额外目录可通过 `GIT_AI_UPLOAD_EXCLUDE_PATH_PREFIXES` 扩展。
6. **同一份时间格式**：commit 的 `%aI` 时间戳在发送前规整为 `yyyy-MM-dd HH:mm:ss`。

也有两处需要在文档里明确的差异：

1. **触发时机不同**：脚本上传是“扫描一批 commit 再发 batch”；原生上传是“每次 commit 成功后先放入当前 commit，再合并本地已登记的 `failed` / `not_uploaded` note 组成有上限的补传 batch”。
2. **`source` 字段不同**：当前原生实现固定写 `source="auto"`，用于区分脚本路径的 `manual` 和 Code Review 路径的 `codeReview`。

#### 3.2.3 原生上传的失败策略

`git-ai` 侧当前实现明确采用 **best-effort** 原则：

1. feature flag 未开启，直接跳过。
2. 当前 commit 没算出 `CommitStats`（例如 merge commit 或被 expensive 保护跳过），会把对应 note 标为 `not_uploaded`，等待后续上传入口补传。
3. payload 组装失败、网络失败、接口返回非 2xx，只输出 debug 日志，不中断 commit；已进入本次 batch 的 note 会写入 `.git/ai/upload_stats_status.json`，并标成 `failed` 供下次重试。
4. 上传成功后，本次 batch 里的所有 note 标成 `succeeded`；如果本地 note blob id 后续变化，状态自动回到 `not_uploaded`，避免上传状态与实际 note 内容脱节。
5. 首次扫描发现的历史 note 只先登记状态，不立即全部拖进当前 batch；后续上传按 `GIT_AI_UPLOAD_BACKLOG_LIMIT`（默认 25）逐步补传，失败记录优先于未上传记录。
6. async daemon 路径由后台服务执行上传；sync wrapper 路径以内联方式完成上传，避免宿主进程过快退出导致上传线程被带死。HTTP timeout 为 20 秒，上传失败不影响 commit 成功。
7. async daemon 启动阶段如果遇到 `daemon.lock` 被占用但 control / trace socket 不可用，`git-ai` 会先等待同伴进程完成启动；若超时且能从 `daemon.pid.json` 读到 pid，会自动强制回收旧 daemon 并拉起新 daemon。若 Windows 拒绝结束旧进程（例如旧 daemon 由更高权限进程拉起），当前实现会保留 blocked startup 错误并拒绝激活 replacement runtime，避免多个 `bg run` 在不同 runtime 下继续并存、共同争用全局 `upload_activity.lock`。

### 3.3 路径 B 详细实施：`upload-ai-stats.ps1` 主动上传脚本

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

### 3.4 路径 C 详细实施：Code Review 时自动上传

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
| AI归因新增 | `stats.aiAdditions` | 当前默认对外展示的 AI 新增行数，只统计最终落地且带 AI attestation 的新增行，不包含 `mixedAdditions` |
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
    "clientContext": {
        "gitAiCliVersion": "1.3.5",
        "gitAiPluginVersion": "0.9.2",
        "ideName": "VS Code",
        "ideVersion": "1.100.2",
        "gitVersion": "2.49.0.windows.1"
    },
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
| `clientContext` | 本次上传所在客户端环境信息 | 本地 `git-ai` / IDE / Git 运行时采集 | 用于排查归因异常、插件兼容性和版本分布 |
| `commits` | 本次批量提交的 commit 列表 | 客户端组装 | 每个元素仍然对应一个 commit |

**`clientContext` 字段说明：**

| 字段 | 含义 | 来源 | 备注 |
|------|------|------|------|
| `gitAiCliVersion` | 当前执行上传的 `git-ai` CLI 版本号 | `git-ai --version` | 建议保留完整版本字符串，便于区分 debug / release |
| `gitAiPluginVersion` | 当前 IDE 内 git-ai 集成插件/扩展版本号 | 优先读 `GIT_AI_REPORT_PLUGIN_VERSION` | 无插件链路时可为 `null` |
| `ideName` | 当前使用的 IDE / 编辑器名称 | 优先读 `GIT_AI_REPORT_IDE_NAME`，否则回退 `TERM_PROGRAM` | 例如 `VS Code`、`Cursor`、`IntelliJ IDEA` |
| `ideVersion` | 当前 IDE / 编辑器版本号 | 优先读 `GIT_AI_REPORT_IDE_VERSION`，否则回退 `TERM_PROGRAM_VERSION` | CLI-only 场景可为 `null` 或约定值 |
| `gitVersion` | 当前真实 Git 版本号 | `git --version` 的运行时结果 | 用于排查 hooks / trace2 / wrapper 兼容性 |

> **实现补充（2026-05-08）**：`git-ai/src/integration/upload_stats.rs` 与 `spec-kit/scripts/powershell/upload-ai-stats.ps1` 现在都已按上表把 `clientContext` 落到上传 payload 顶层；其中 `gitAiCliVersion` 与 `gitVersion` 自动采集，`gitAiPluginVersion` / `ideName` / `ideVersion` 支持通过 `GIT_AI_REPORT_PLUGIN_VERSION`、`GIT_AI_REPORT_IDE_NAME`、`GIT_AI_REPORT_IDE_VERSION` 显式传入。批量脚本在未显式提供 IDE 名称/版本时，还会继续回退 `TERM_PROGRAM` / `TERM_PROGRAM_VERSION`。

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
| `aiAdditions` | 带有 AI 归因的新增行数；按最终落地 attestation 统计，当前等于 `aiAccepted`，不再并入 `mixedAdditions` | 80 行 |
| `aiAccepted` | AI 生成且最终未被人工改动的行数 | 65 行 |
| `mixedAdditions` | 过程指标，表示 AI 输出在提交前被人工覆盖/混合编辑过的行数；不是最终 AI 归因新增行 | 15 行 |
| `totalAiAdditions` | 本次开发过程中 AI 一共生成过多少行，可能大于最终提交中的 `aiAdditions` | 95 行 |
| `totalAiDeletions` | AI 参与的删除行数 | 10 行 |
| `gitDiffAddedLines` | git diff 统计的总新增行数 | 200 行 |
| `gitDiffDeletedLines` | git diff 统计的总删除行数 | 30 行 |
| `timeWaitingForAi` | 等待 AI 响应的总时间（秒） | 45 秒 |
| `files` | 逐文件 commit-local 归因明细数组；由 `Get-CommitAiFileStats` 解析 authorship note attestation + `git diff-tree --numstat` 产出 | 见上面 JSON 示例 |
| `toolModelBreakdown` | 按工具+模型分组的细分数据数组；每项包含 `tool`、`model`、`aiAdditions`、`aiAccepted`、`mixedAdditions` | 见上面 JSON 示例 |

**`stats.files[]` 内部字段详解：**

> **数据来源：** `git diff-tree --no-commit-id --numstat -r <sha>`（行数）+ `git notes --ref=ai show <sha>`（attestation 归因）。这是 commit-local 语义，只反映当前 commit 自身的 AI/人工归因，不做跨 commit 的 provenance 追溯。

> **实现约束补充（2026-05-04）：** 读取 `git diff-tree --numstat` 时必须关闭 Git 默认的路径转义，即使用 `git -c core.quotepath=false diff-tree ...`。否则中文等非 ASCII 文件名会被转成 `"...\347..."` 形式，导致它和 authorship note 中的 UTF-8 `attestation.file_path` 键不相等，最终把该文件的 `aiAdditions` 误记为 `0`。

> **默认过滤规则：** `target/` 下的构建产物不会进入 `stats.files[]`。如果团队还有其他需要排除的目录，可以设置 `GIT_AI_UPLOAD_EXCLUDE_PATH_PREFIXES`，使用逗号或分号分隔多个路径前缀。

| files 字段 | 含义 | 举例 |
|------------|------|------|
| `filePath` | 文件路径（来自 `git diff-tree --numstat`） | `src/auth/service/AuthService.java` |
| `gitDiffAddedLines` | 该文件在本次 commit 中的新增行数（来自 `git diff-tree --numstat`） | 120 行 |
| `gitDiffDeletedLines` | 该文件在本次 commit 中的删除行数（来自 `git diff-tree --numstat`） | 18 行 |
| `aiAdditions` | 该文件中 AI 归因新增行数（来自 authorship note attestation，非 `h_*` 前缀的条目） | 55 行 |
| `humanAdditions` | 该文件中已知人工新增行数（来自 authorship note attestation，`h_*` 前缀的条目） | 45 行 |
| `unknownAdditions` | 该文件中未归因新增行数（`gitDiffAddedLines - aiAdditions - humanAdditions`） | 20 行 |
| `toolModelBreakdown` | 该文件维度的工具/模型分解（来自 authorship note JSON 元数据的 `prompts.<hash>.agent_id`）；当前稳定提供 `aiAdditions` | 见上面 JSON 示例 |

### 3.5.1 git-ai 统计字段可靠性口径

**结论先放前面：** 看板核心 KPI 建议优先使用 Git 原生字段、`hasAuthorshipNote`、commit-local 的 diff 行数，以及 authorship note 能直接证明的 AI / 人工 / unknown 新增行。prompt 正文、等待时间和“过程中生成过多少行”这类字段可以展示，但不建议作为强考核指标。

| 字段 / 数据 | 可靠性 | 可用于看板核心统计吗 | 口径说明 |
|-------------|--------|----------------------|----------|
| `commitSha` / `commitMessage` / `author` / `timestamp` | 高 | 是 | 来自 `git log` / commit 对象，是提交事实本身 |
| `hasAuthorshipNote` | 高 | 是 | 来自 `git notes --ref=ai list <sha>`；它只能证明“是否存在归因 note”，不能证明“没有 note 就没有 AI” |
| `gitDiffAddedLines` / `gitDiffDeletedLines` | 高 | 是 | 来自 `git diff-tree --numstat`，反映该 commit 自身的新增 / 删除行数；二进制文件或被排除目录需单独处理 |
| `stats.files[].filePath` / 文件级 diff 行数 | 高 | 是 | 文件路径和新增 / 删除来自 `git diff-tree --numstat`，是 commit-local 口径 |
| `aiAdditions` | 高（有 note 且 checkpoint 链完整时） | 是 | 来自 authorship note attestation 中非 `h_*` 的归因条目，表示该 commit 中可归因到 AI 的新增行 |
| `humanAdditions` | 高（有 note 且 checkpoint 链完整时） | 是 | 来自 `h_*` / `KnownHuman` 等人工归因条目，表示已知人工新增行 |
| `unknownAdditions` | 高，但含义要谨慎 | 是，需单独展示 | 可靠含义是“未归因新增行”，不是“人工行”；没有 note 或 checkpoint 缺失时会升高 |
| `aiAccepted` / `mixedAdditions` | 中高 | `aiAccepted` 可参与主指标，`mixedAdditions` 仅辅助展示 | `aiAccepted` 是最终落地的 AI 行；`mixedAdditions` 是过程指标，不应并入 AI 或人工主计数 |
| `toolModelBreakdown.tool` / `model` | 中高 | 可展示 | 来自 note JSON 元数据里的 `prompts.<hash>.agent_id`；没有 prompt 元数据或旧 note 时可能为空 |
| `prompts[]` / `promptText` | 中 | 可展示，不建议做强 KPI | 只有 `prompt_storage=notes` 且脱敏后 messages 保留在 note 中时稳定；`default` / `local` 下可能为空 |
| `timeWaitingForAi` | 低到中 | 仅辅助参考 | 依赖工具 transcript / telemetry 是否提供等待时间，缺失或口径不一时不适合横向考核 |
| `totalAiAdditions` / `totalAiDeletions` | 中 | 仅辅助参考 | 更像过程 / 历史参与量，可能大于最终 commit 中的 `aiAdditions`，不适合替代最终提交占比 |

**看板建议口径：**

1. AI 占比主指标使用 `aiAdditions / gitDiffAddedLines`，并单独展示 `unknownAdditions`。
2. 人工占比主指标只统计 `humanAdditions`，不要把 `unknownAdditions` 自动并入人工。
3. 逐文件详情使用 `stats.files[]`，因为它已经明确是 commit-local 语义。
4. `mixedAdditions` 只作为“AI 输出曾被人工改写”的过程参考，不参与 AI 占比、人工占比和文件最终归因。
5. prompt 明细只作为排查和上下文展示；若 `promptText` 为空，先检查本机 `prompt_storage` 是否为 `notes`。

**响应体 `results[]` 字段说明：**

| 字段 | 含义 | 备注 |
|------|------|------|
| `commitSha` | 这条结果对应的 commit | 用于和客户端原始请求一一对应 |
| `succeeded` | 单个 commit 是否处理成功 | Java DTO 可直接映射为布尔字段 |
| `status` | 处理状态 | 建议值：`upserted`、`failed`、`skipped` |
| `errorMessage` | 失败原因 | 仅失败时返回 |

---

### 3.6 配置项（环境变量 + IDE MCP X-USER-ID 自动探测）

**要做什么：** 通过环境变量告诉上传链路"往哪里发"和"用什么认证"。

> **与最新 git-ai 原生能力的边界：** `GIT_AI_REPORT_REMOTE_URL` / `GIT_AI_REPORT_REMOTE_ENDPOINT` / `GIT_AI_REPORT_REMOTE_PATH` / `GIT_AI_REPORT_REMOTE_API_KEY` / `GIT_AI_REPORT_REMOTE_USER_ID` / `GIT_AI_VSCODE_MCP_CONFIG_PATH` / `GIT_AI_IDEA_MCP_CONFIG_PATH` / `GIT_AI_REPORT_PLUGIN_VERSION` / `GIT_AI_REPORT_IDE_NAME` / `GIT_AI_REPORT_IDE_VERSION` 现在同时服务于两条路径：`git-ai` 原生自动上传，以及本文的自建上传脚本路径；如果改用 Git AI 官方或自托管后端，应改配 `GIT_AI_API_KEY`，必要时再配 `GIT_AI_API_BASE_URL`。

**为什么不用 `git-ai config set`？**
- 当前 `git-ai config` 不支持 `report_to_remote.endpoint` / `report_to_remote.api_key` 这类自定义键
- 当前 `git-ai` 原生上传和上传脚本都已经统一读取同一组环境变量，避免再引入一套新的配置源
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

**补充配置：prompt 存储模式默认切到 `notes`**

如果当前目标是“快速实现提示词正文随现有上传链路一起落库”，`git-ai` 的运行时默认值已经改为 `prompt_storage=notes`。也就是说，新用户没有显式配置时，脱敏后的用户输入 prompt messages 会默认保留在 `refs/notes/ai` 中，现有上传脚本和 Rust 原生自动上传都能直接提取 `promptText`。

三种模式的差异如下：

| 模式 | prompt 保存位置 | note 中是否保留 `messages` | 对当前 `promptText` 落库的影响 |
|------|----------------|----------------------------|------------------------------|
| `default` | 优先 CAS / prompt store；note 中通常只剩 `messagesUrl` 或更少元数据 | 否 | 当前上传端如果不额外回查 CAS，则 `promptText` 很容易为空 |
| `local` | 仅本地 SQLite | 否 | 当前上传端不会自动读本地 SQLite，因此 `promptText` 仍容易为空 |
| `notes` | 直接保存在 `refs/notes/ai` | 是 | 现有上传链路可直接提取用户输入 `promptText`，实现成本最低 |

**当前默认：** 新配置默认使用 `notes`。如果本机已有 `prompt_storage=default` 或 `prompt_storage=local`，仍会尊重已有配置；需要统一校正时再执行下面命令。

**配置命令：**

```powershell
git-ai config set prompt_storage notes
git-ai config prompt_storage
```

期望输出：

```text
notes
```

如果未来要回到更严格的隐私边界，再显式切回 `default`，并补一条“上传时根据 `messagesUrl` / CAS / 本地 SQLite 回填 prompt 正文”的链路即可。

**为什么这里明确选择 `notes`，而不是继续保持 `default`：**

1. 当前服务端 `prompt_text` 的缺口不是表结构问题，而是上传端拿到的 note 里 `messages` 已经被清空。
2. 现有上传脚本和 Rust 原生自动上传，都是直接从 note 的 `prompts[].messages` 中 `Message::User` 记录提取 `promptText`。
3. 切到 `notes` 后，现有链路基本不用再改，就能让 `prompt_text` 随同上传请求一起进入 `git_ai_prompt_stats`。
4. 当前代码已经把缺省值和非法值回退都改成 `notes`，避免新装用户因为没配 `prompt_storage` 导致 prompt 正文为空。

**验收标准：**

1. 本地执行 `git notes --ref=ai show <sha>` 时，`prompts.<hash>.messages` 非空。
2. 上传后查询 `cr.git_ai_prompt_stats`，`prompt_text` 不再是空值。

**注意：** 这条方案的代价已经从“完整 prompt 会随 git notes 一起流转”收紧为“用户输入 prompt 会随 git notes 一起流转”。虽然 `git-ai` 在 `notes` 模式下会先做脱敏，且当前实现不会再保留 assistant/tool transcript，但它仍然比 `default` / `local` 更外显。当前阶段因为目标是“先快速实现”，这个取舍是可以接受的；如果后续要继续收紧提示词可见性，再补 CAS / prompt store 回填方案。

**debug 诊断日志：**

checkpoint、post-commit stats 和上传链路现在默认追加 JSONL 到：

```text
~/.git-ai/logs/debug.jsonl
```

关闭文件日志时显式设置：

```powershell
$env:GIT_AI_DEBUG = "false"
[Environment]::SetEnvironmentVariable("GIT_AI_DEBUG", "false", "User")
```

如果只想在终端 stderr 看到部分调试输出，再单独开启：

```powershell
$env:GIT_AI_DEBUG_STDERR = "1"
```

每条 JSONL 日志都会带 `timestampMs` 和北京时间 `timestamp`（固定 `UTC+08:00`）。当 `debug.jsonl` 超过 2GB 时，`git-ai` 会尽力保留最近约 512MB 内容后继续写入；日志创建、写入或裁剪失败都静默忽略，不允许影响 commit、stats 或上传主流程。

关键事件如下：

| 事件 | 用途 |
|------|------|
| `checkpoint_skipped` | checkpoint 未执行，记录原因、请求的 `kind`、是否带 agent 上下文 |
| `checkpoint_explicit_path_resolution` | agent/IDE 带了显式路径但部分或全部未进入 checkpoint；逐路径记录 `included` / `dropped` 和 `duplicate_explicit_path`、`ignored_by_git_ai_ignore_rules`、`outside_repository_workdir`、`clean_file_without_dirty_snapshot`、`not_a_text_file_or_missing` 等原因 |
| `checkpoint_no_entries` | 找到了候选文件但没有写入归因 entry；现在包含 `fileDiagnostics[]`，逐文件记录 `outcome=skipped`、`reason`、上一条 `previousCheckpoint`、内容是否相同和 `rootCauseHint` |
| `checkpoint_attribution_decision` | checkpoint 实际写入后的归因判定；记录 `kind`、`decision`、`isAi`、`decisionRule`、文件样例、行数、agent 工具/模型、显式捕获路径、`fileDiagnostics[]` 和 `diagnosticHints` |
| `known_human_checkpoint_rejected` | KnownHuman 保存事件被 AI 保存抑制规则拦截；记录拦截窗口、候选文件和命中的上一条 AI checkpoint，证明抑制规则已生效 |
| `post_commit_started` | post-commit 主流程开始，记录 repo、commit、parent、human author、是否有最终文件状态覆盖 |
| `post_commit_working_log_loaded` | 已读取并刷新 working log，记录 checkpoint / entry 数量 |
| `post_commit_pathspecs_prepared` | 已根据 AI 相关 checkpoint 和 INITIAL 归因准备 post-commit pathspec |
| `post_commit_authorship_log_built` | authorship log 已构建，记录 attestation、prompt 和 INITIAL 结转摘要 |
| `post_commit_authorship_note_write_started` | 准备写入 authorship note，记录 note 字节数 |
| `post_commit_authorship_note_written` | authorship note 写入成功，记录 prompt 保存模式、note 大小、prompt/message 计数 |
| `post_commit_stats_compute_started` | 准备计算 commit stats，记录 ignore pattern 数量 |
| `post_commit_stats_computed` | commit stats 已计算，记录 AI/人工/unknown/add/delete 汇总 |
| `post_commit_stats_skipped` | stats 被跳过，记录 `merge_commit` 或 `expensive_commit` 原因；这会导致本次自动上传跳过 |
| `post_commit_initial_attributions_written` | 有未提交归因需要结转到新 base commit 时记录写入结果 |
| `post_commit_working_log_deleted` | 旧 base commit 的 working log 已清理 |
| `post_commit_upload_dispatch_requested` | post-commit 准备把 authorship note 和 stats 交给上传模块；记录本次是否有 stats、是否因跳过导致上传不可用 |
| `upload_stats_auto_entered` | 自动上传入口被调用，记录 feature flag、async mode、stats 是否可用 |
| `upload_stats_manual_entered` | 手动 `upload-stats` 入口被调用，记录 dry-run 和 ignore pattern 数量 |
| `upload_stats_manual_authorship_note_loaded` | 手动上传已读取到 authorship note |
| `upload_stats_manual_stats_computed` | 手动上传已重新计算 stats |
| `upload_stats_payload_build_started` | 开始构造上传 payload，记录 stats/prompt/attestation 摘要 |
| `upload_stats_payload_build_succeeded` | payload 构造完成，记录 payload 摘要 |
| `upload_stats_skipped` | 上传前跳过，记录 feature flag 关闭、stats 缺失或 payload 构建失败等原因 |
| `upload_stats_ready` | payload 已生成，记录上传模式、URL 来源、是否有 API Key / X-USER-ID、payload 摘要 |
| `upload_stats_dry_run_prepared` | 手动 dry-run 已完成，不会真正发 HTTP |
| `upload_stats_activity_lock_wait_started` | 开始等待全局上传 activity lock，记录 lock 路径、模式和最长等待时间 |
| `upload_stats_activity_lock_acquired` | 已拿到上传 activity lock，记录实际等待时间 |
| `upload_stats_activity_lock_timeout` | 等待上传 activity lock 超时；如果后续没有 `upload_stats_activity_lock_bypassed` 或 HTTP 事件，说明失败仍停在本地锁之前 |
| `upload_stats_activity_lock_bypassed` | 自动上传在 activity lock 超时后按 best-effort 继续发送 HTTP；手动 `upload-stats` 不会产生该事件 |
| `upload_stats_started` | HTTP 上传开始，记录模式、URL 和 payload 摘要 |
| `upload_stats_http_prepare_started` | HTTP agent/request 准备开始，记录 timeout、URL、是否带 API key / user id |
| `upload_stats_http_body_serialize_failed` | payload 序列化请求体失败 |
| `upload_stats_http_request_ready` | 请求体已序列化完成，记录 body 字节数，不记录 body 内容 |
| `upload_stats_http_send_failed` | HTTP 请求发送阶段失败，通常是网络、DNS、TLS 或连接问题 |
| `upload_stats_http_response_received` | 收到 HTTP 响应，记录状态码和是否 2xx |
| `upload_stats_http_non_success` | 收到非 2xx 响应，记录状态码和响应正文摘要 |
| `upload_stats_failed` | HTTP 上传失败，记录错误信息；非 2xx 会包含 HTTP 状态和响应正文摘要 |
| `upload_stats_succeeded` | HTTP 上传成功，记录 HTTP 状态码 |

GitHub Copilot VS Code native hook 的补充说明：当 hook payload 因为脱敏而只剩 `tool_input="..."`、`tool_response=""` 时，当前实现除了原始 `tool_use_id == toolCallId` 外，还兼容一侧带 `__vscode-<digits>` 后缀、另一侧不带后缀的同一次工具调用。这个兼容仍然只针对 transcript 中的单次精确 tool call，不会退化成扫描整段会话历史。

显式路径过滤的补充说明：当 `checkpoint_explicit_path_resolution` 里 `explicitCapture.role=WillEdit`，并且目标文件是仓库内当前存在的文本文件时，2026-05-04 之后的实现不应再把它记成 `clean_file_without_dirty_snapshot`。如果仍然出现这个诊断，优先怀疑运行的还是旧二进制，或者这次显式路径实际上不是 `WillEdit` 而是 `Edited`。

排查口径：AI 代码被标成人工时，优先看同一次 `checkpoint_attribution_decision.kind` 是否为 `human` / `known_human`，以及 `diagnosticHints` 是否出现 `human_fallback_no_agent_context`、`known_human_save_checkpoint` 或 `check_whether_ai_save_suppression_missed_this_path`；再看 `fileDiagnostics[].reason`。如果后续 AI checkpoint 的 `checkpoint_no_entries.fileDiagnostics[].reason=unchanged_from_previous_checkpoint`，并且 `previousCheckpoint.kind=known_human`，同时 `rootCauseHint=ai_checkpoint_arrived_after_known_human_already_captured_same_file`，就可以直接判定为 KnownHuman 先捕获了这批代码。如果没有进入 `checkpoint_no_entries`，先看是否有 `checkpoint_explicit_path_resolution`，它能说明显式路径是否被 ignore、越界、clean、非文本或缺少 dirty snapshot。人工代码被标成 AI 时，看 `kind=ai_agent` / `ai_tab`、`agent.tool`、`agent.model`、`editedFilepathCount`、`editedFilepathsSample`、`willEditFilepathsSample`、`dirtyFilePathsSample`、`explicitCapture` 是否覆盖了该文件；统计未上传时，看 `post_commit_stats_skipped`、`upload_stats_skipped`、`upload_stats_ready` 和 `upload_stats_failed` 的原因、URL 来源、用户 ID/API Key 状态和 HTTP 错误。

典型误归因链路：如果同一 `baseCommit` / 文件集合先出现 `checkpoint_attribution_decision.kind=known_human`、`decision=human`、`isAi=false`，并带有 `known_human_save_checkpoint` / `check_whether_ai_save_suppression_missed_this_path`，随后 `ai_agent` 对同一批文件只出现 `checkpoint_no_entries`，且逐文件 `fileDiagnostics[].previousCheckpoint.kind=known_human`、`reason=unchanged_from_previous_checkpoint`，说明 IDE 的已知人工保存 checkpoint 先把当前文件状态写进 working log，后续 AI checkpoint 因为文件已无新增差异而没有机会覆盖。最终 `post_commit_stats_computed` / `upload_stats_ready` 里出现 `aiAdditions=0` 不是远程上传或看板计算错误，而是本地 authorship note 已经按人工归因生成。继续向上查时，看 `agent.knownHumanMetadata.editor` / `editorVersion` / `extensionVersion`、`editedFilepathsSample`、`willEditFilepathsSample`、`dirtyFilePathsSample` 来确认 KnownHuman 事件来源；如果出现 `known_human_checkpoint_rejected`，说明 AI 保存抑制规则已拦截该次保存事件，误归因应继续排查其他 checkpoint 或路径匹配问题。2026-05-04 之后，运行新二进制时，这条链路在“最近一条是 IDE `KnownHuman`、AI checkpoint 内容相同”的场景下不应再直接停留在 `checkpoint_no_entries`；它应该改为基于更早的 previous state 写出 AI entry。若仍然看到这组老信号，优先怀疑运行的还是旧二进制，或异步模式下这条 AI checkpoint 还没有真正处理完成。

这份日志只用于本机排查，不参与上传；失败时静默跳过，不能影响 commit。环境变量、config、debug 日志查看和设置方式见 `docs/design-doc/git-ai环境变量与配置说明.md`。

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

第 2 步：执行 Speckit 初始化（CLI 自动触发 post-init）
  ┌─────────────────────────────────────────┐
    │  specify init . --ai copilot             │
    │                                          │
    │  或更新已有项目：                         │
    │  specify init --here --force --ai copilot│
  └─────────────────────────────────────────┘
    CLI 内部执行顺序：
        ① 完成 Spec Kit 模板/脚本初始化
        ② 保存 init-options.json
        ③ 安装 preset（如果有）
        ④ 自动执行 .specify/scripts/powershell/post-init.ps1
  
    post-init 脚本内部执行顺序：
        ① 检测 git-ai 命令，或回退检查默认安装路径 ~/.git-ai/bin
        ② CLI 以 force-update 语义调用 post-init，因此无论本机是否已安装，都会从目标安装源重新拉取 git-ai
              （当前默认安装器地址已经固定为 `https://raw.githubusercontent.com/rj-gaoang/git-ai/main/install.ps1`，也支持 `GIT_AI_INSTALLER_URL` 覆盖；未显式覆盖时，post-init 还会自动补齐 `GIT_AI_GITHUB_REPO=rj-gaoang/git-ai` 与 `GIT_AI_RELEASE_TAG=latest`，因此 installer 脚本走源码、二进制仍走 latest release）
          ③ 安装完成后执行 `git-ai install-hooks` → 刷新 IDE / Agent hooks 配置
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

**当前推荐路径：**
- 少量 commit 或只想手动补传当前结果时，优先使用 `git-ai upload-stats`。
- 需要按日期范围批量回补时，继续使用 `.specify/scripts/powershell/upload-ai-stats.ps1`。

**完整步骤：**

```
第 1 步：确认自己在功能分支上
  ┌───────────────────────────────────────┐
  │  git branch                           │
  │  # * 001-user-auth  ← 当前分支        │
  └───────────────────────────────────────┘

第 2 步：先预览看看有什么数据（推荐）
    ┌───────────────────────────────────────────────────────────────┐
    │  git-ai upload-stats --dry-run                                │
    └───────────────────────────────────────────────────────────────┘
    预期输出：
        [git-ai] upload-stats: dry-run abc1234 source=manual ...
        [git-ai] upload-stats: completed source=manual uploaded=0 dry_run=1 skipped=0 failed=0

第 3 步：确认无误后，真正上传
    ┌───────────────────────────────────────────────────────────────┐
    │  git-ai upload-stats                                          │
    └───────────────────────────────────────────────────────────────┘
    预期输出：
        [git-ai] upload-stats: uploaded abc1234 source=manual status=200 ...
        [git-ai] upload-stats: completed source=manual uploaded=1 dry_run=0 skipped=0 failed=0

第 4 步：如果需要按范围批量回补，再使用脚本方案
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

第 5 步：确认无误后，真正执行脚本批量上传
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

第 6 步：提交 PR / MR（正常流程）
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

### Phase 1（1-2 天）：让 Speckit 自动安装 git-ai

**目标：** 新成员 clone 仓库后，git-ai 自动到位，不需要手动安装。

| 步骤 | 具体操作 | 要修改/创建的文件 | 怎么验证做完了 |
|------|---------|------------------|---------------|
| 1.1 | 创建 post-init 模板源脚本 | `spec-kit/scripts/powershell/post-init.ps1` + `spec-kit/scripts/bash/post-init.sh` | `specify init` / `specify init --script sh` 都能把脚本带进新项目 |
| 1.2 | 同步仓库内副本 | `spec-kit/.specify/scripts/powershell/post-init.ps1` + `spec-kit/test-verify/.specify/scripts/powershell/post-init.ps1` | 仓库内自举目录和验证目录行为一致 |
| 1.3 | 修改 Spec Kit upstream 的 `init()` 流程，并以 force-update 语义调用 post-init | `spec-kit/src/specify_cli/__init__.py` | 执行 `specify init . --ai copilot` 或 `specify init --here --force --ai copilot` 后都会触发 git-ai 更新 |
| 1.4 | 支持 git-ai installer 在运行时覆盖目标 GitHub 仓库 / release tag | `git-ai/install.ps1` + `git-ai/install.sh` | 设置 `GIT_AI_GITHUB_REPO` / `GIT_AI_RELEASE_TAG` 后，安装日志能显示目标 repo / release |
| 1.5 | 在 `git commit` 成功后复用现有后台升级链路，补齐 CLI 自更新触发点 | `git-ai/src/commands/git_handlers.rs` | `cargo test --lib post_commit_followups_require_successful_commit` 通过，且 commit 路径只在成功提交时调度后台升级检查，不阻塞本次提交 |
| 1.6 | （可选）按本文「需求 1 步骤 3」修改 check-prerequisites.ps1 | `.specify/scripts/powershell/check-prerequisites.ps1` | 存量旧项目中也能提示 git-ai 缺失 |

### Phase 2（2-4 天）：实现上传能力（原生自动 + 主动批量）

**目标：** 同时具备“commit 后即时上报”和“按范围批量回补”两种能力。

| 步骤 | 具体操作 | 要修改/创建的文件 | 怎么验证做完了 |
|------|---------|------------------|---------------|
| 2.1 | 按本文「需求 2 - 3.3」创建 / 维护 `upload-ai-stats.ps1` | `.specify/scripts/powershell/upload-ai-stats.ps1` | `upload-ai-stats.ps1 -DryRun` 能看到 commit 列表和 AI 统计预览 |
| 2.2 | 确认 `git-ai stats <sha> --json` 输出格式满足脚本和审查路径需求 | 无需修改，只验证 `src/authorship/stats.rs` | 对任意有 AI note 的 commit 执行，JSON 输出包含所有需要的字段 |
| 2.3 | 在 git-ai 中新增原生上传模块和 feature flag | `git-ai/src/integration/upload_stats.rs` + `git-ai/src/integration/mod.rs` + `git-ai/src/feature_flags.rs` | `cargo test --lib integration::upload_stats` 通过，且能看到 `auto_upload_ai_stats` 已生效 |
| 2.4 | 将原生上传挂到 `post_commit` | `git-ai/src/authorship/post_commit.rs` | 设置 `GIT_AI_AUTO_UPLOAD_AI_STATS=true` 后，commit 完成可看到上传 debug 日志 |
| 2.5 | 按本文「3.6 / 7」配置 URL、api_key、user_id | 用户环境变量 / CI Secret | 新开 shell 后能读取到 `GIT_AI_REPORT_REMOTE_API_KEY`；如显式写入 URL，则 `GIT_AI_REPORT_REMOTE_URL` 也能读取 |
| 2.6 | 代码级验证与真实上传测试 | 需要远程 API 服务可用 | `cargo build` / `cargo clippy -D warnings` / `cargo fmt` 通过，且接口返回成功 |
| 2.7 | 将 prompt 默认存储改为 notes | `git-ai/src/config.rs` | 未配置 `prompt_storage` 时，运行时默认返回 `notes`，`git_ai_prompt_stats.prompt_text` 可从 note 中稳定提取 |
| 2.8 | 增加 debug 模式本地诊断日志 | `git-ai/src/diagnostics.rs`、`git-ai/src/commands/checkpoint.rs`、`git-ai/src/authorship/post_commit.rs`、`git-ai/src/integration/upload_stats.rs` | 设置 `GIT_AI_DEBUG=1` 后，checkpoint 归因、post-commit stats 和上传链路追加 `~/.git-ai/logs/debug.jsonl` |
| 2.9 | 增加原生主动上传命令 | `git-ai/src/commands/git_ai_handlers.rs`、`git-ai/src/integration/upload_stats.rs` | 执行 `git-ai upload-stats --dry-run` 能预览单个 commit 的本地上传结果，执行 `git-ai upload-stats` 能显式上传 |
| 2.10 | 修复 GitHub Copilot native hook transcript fallback 的 VS Code 后缀兼容 | `git-ai/src/commands/checkpoint_agent/presets/github_copilot/ide.rs` | `tool_input="..."` 且 hook `tool_use_id=call_xxx__vscode-<digits>` 时，仍能命中 transcript 中的 `toolCallId=call_xxx`，生成正确的 `AiAgent` checkpoint |
| 2.11 | 修复显式 `will_edit` 路径在 clean 文本文件上的 checkpoint 过滤误杀 | `git-ai/src/commands/checkpoint.rs` | 当 human pre-hook / `will_edit_filepaths` 已经拿到目标文本文件，但文件当前仍是 clean 状态时，不再被 `clean_file_without_dirty_snapshot` 直接丢弃；`Edited` 显式路径仍保持原有严格过滤 |
| 2.12 | 修复 recent `KnownHuman` 抢先写入后 AI checkpoint 被“无变化”直接跳过 | `git-ai/src/commands/checkpoint.rs` | 当最近一条 previous checkpoint 是来自 IDE 的 `KnownHuman`，且它和当前 AI checkpoint 的文件内容完全一致时，不再继续对着这条 `KnownHuman` 做 `unchanged_from_previous_checkpoint` 判定，而是回退到更早的文件状态 / HEAD 基线重新计算 AI diff；`op-return-exchange` 同步验证结果为 `ai_additions=10`，提交后 `blame` 恢复为 `github-copilot` |
| 2.13 | 强化默认 debug JSONL 与统计上传逐步诊断 | `git-ai/src/diagnostics.rs`、`git-ai/src/authorship/post_commit.rs`、`git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai环境变量与配置说明.md` | `debug.jsonl` 默认开启、带 `timestamp`、超过 2GB 后保留最近日志；上传链路从 post-commit、stats、payload、HTTP request/response 到成功/失败都有逐步事件，排查统计未上传时不再缺关键步骤 |
| 2.14 | 修复 UTF-8 文件名导致的逐文件 AI 统计失真 | `git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | `stats.files[]` 在中文等非 ASCII 文件路径场景下，不再因为 Git `quotepath` 转义而把整文件 `aiAdditions` 错记为 `0` |
| 2.15 | 增加本地上传状态与失败 / 未上传 note 自动补传 | `git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai看板方案.md`、`git-ai/README.md` | `.git/ai/upload_stats_status.json` 记录 `not_uploaded/succeeded/failed`；自动上传和 `git-ai upload-stats` 会把当前 commit 与本地已登记的失败 / 未上传 note 合成有上限的 batch，并在成功 / 失败后更新状态 |
| 2.15 | 修复 whitespace-only 新增行因为未直接落入 attestation range 而被误记为 `unknownAdditions` | `git-ai/src/authorship/stats.rs`、`git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 对“代码块前后带空白分隔行”的提交，`git-ai stats` 和 `stats.files[]` 中的 `humanAdditions` / `aiAdditions` 与 `gitDiffAddedLines` 重新对齐，方法尾部的新增空白行不再单独掉到 `unknown` |
| 2.16 | 固化 Windows 工作区 LLVM Rust / LLVM-MinGW 构建链路，并清理阻塞验证的旧测试签名 | `git-ai/.vscode/settings.json`、`git-ai/src/authorship/stats.rs`、`git-ai/docs/design-doc/git-ai环境变量与配置说明.md`、`git-ai/docs/design-doc/git-ai看板方案.md` | 新开 VS Code 终端后 `cargo -vV` 应显示 `host: x86_64-pc-windows-gnullvm`；执行 `cargo test debug_timestamp_uses_beijing_time --lib` 不再出现 `_Unwind_Resume` / `_GCC_specific_handler`，并能实际跑到测试本身 |
| 2.17 | 修复 `upload-stats` 仅按 HTTP 2xx 误判成功 | `git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 当服务端返回 HTTP 200 但 JSON body 的 `code != 200` 时，自动上传和手动 `upload-stats` 都会记为失败，并把 `code/msg` 写入 `debug.jsonl` |
| 2.18 | 修复 AI follow-up checkpoint 复用被裁剪旧基线时只剩最后几行 AI | `git-ai/src/commands/checkpoint.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 当较早 AI checkpoint 的 `entry.attributions` 已被 working log 裁剪、但后续 AI checkpoint 需要退回这条旧基线继续计算时，会自动用保留下来的 `line_attributions + blob` 还原字符归因；`cargo test test_ai_follow_up_checkpoint_preserves_previous_ai_ranges --lib` 与 `cargo test test_ai_checkpoint_reclaims_recent_known_human_file_capture --lib` 均通过 |
| 2.19 | prompt 持久化与上传仅保留用户输入，不再携带 assistant transcript | `git-ai/src/authorship/secrets.rs`、`git-ai/src/authorship/post_commit.rs`、`git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | `post_commit` 写 note/CAS/local 前会把 `PromptRecord.messages` 过滤成仅 `Message::User`；上传 payload 的 `messages[]` 也只序列化用户输入；`cargo test retain_user_prompt_messages_drops_non_user_entries --lib` 与 `cargo test build_prompt_stats_includes_prompt_text_and_messages --lib` 均通过 |
| 2.20 | 修复 session/trace checkpoint 只落 `sessions` 不落 `prompts` | `git-ai/src/authorship/virtual_attribution.rs` | session/trace 形态提交后的 authorship note 会同时包含 `metadata.sessions` 与 `metadata.prompts`；`cargo test record_checkpoint_agent_metadata_for_session_format_creates_prompt_and_session --lib` 与 `cargo test calculate_and_update_prompt_metrics_supports_session_trace_prompt_ids --lib` 均通过 |
| 2.21 | Windows 现场切换 Copilot native hook 到目标新版 binary，避免旧安装版重新拉起旧 daemon | 用户机 `~/.copilot/hooks/git-ai.json` + 目标 `git-ai.exe` | 当旧安装版 `git-ai.exe` 被锁、replacement runtime 已接管 trace2 后，下一次 Copilot AI 编辑与 commit 仍会由 hook JSON 指向的 binary 决定；将 hook 命令切到目标新版 binary 后，`debug.jsonl` 中 `post_commit_*` 的 `processId` 应对应目标 binary 路径 |
| 2.22 | 自动上传遇到 `upload_activity.lock` 超时时继续 best-effort 发送 HTTP | `git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 黄芳 `debug(11).jsonl` 这类“payload 已生成但 HTTP 前卡在 activity lock”的场景下，自动上传会写入 `upload_stats_activity_lock_bypassed` 并继续发送；手动 `upload-stats` 仍严格等待锁 |
| 2.23 | 安装成功后发送远程看板测试数据，且不能因 MCP 用户 ID 缺失跳过 | `git-ai/src/integration/install_test_upload.rs`、`git-ai/src/integration/mod.rs`、`git-ai/src/commands/install_hooks.rs` | `install-hooks` / 安装成功后写入 `install_test_upload_*` debug 事件；身份按 MCP / 环境用户 ID、Git 邮箱、IP 地址兜底；payload 包含 git-ai 版本、Git 版本、操作系统和版本，且所有 tool/prompt 明细补齐 `model=unknown` |
| 2.24 | 无 authorship note 的 commit 不再跳过上传 | `git-ai/src/integration/upload_stats.rs`、`git-ai/src/commands/git_ai_handlers.rs`、`git-ai/src/commands/git_handlers.rs` | `git-ai upload-stats <sha>` 对无 note commit 构造 `hasAuthorshipNote=false`、`prompts=[]`、`unknownAdditions=gitDiffAddedLines` 的 metadata-only payload；commit wrapper 会等待 note，超时仍无 note 时后台触发兜底上传 |
| 2.25 | 远程 payload 时间统一转换为北京时间 | `git-ai/src/integration/upload_stats.rs`、`git-ai/src/integration/install_test_upload.rs` | `%aI` 先按原始时区解析，再转 `UTC+08` 并格式化为 `yyyy-MM-dd HH:mm:ss`；后端不再收到 RFC3339 纳秒格式，也不会把 `2026-06-14T05:13:25+00:00` 误传成 `2026-06-14 05:13:25` |
| 2.26 | `install-hooks` 自动修复失效全局 `core.hooksPath` 并清理旧 daemon | `git-ai/src/commands/install_hooks.rs` | 如果全局 `core.hooksPath` 指向不存在的历史托管目录，如 `ai-contribution-tracker` / `.git-ai` / `.git/ai/hooks`，安装时自动删除；Windows 重启 daemon 前按安装目录兜底清理旧 `git-ai.exe bg run`，避免旧进程继续处理 Trace2 / post-commit |
| 2.27 | 弱 KnownHuman 不再把无 AI checkpoint 的新增代码强归人工 | `git-ai/src/daemon/checkpoint.rs`、`git-ai/src/authorship/stats.rs`、`git-ai/src/commands/checkpoint_agent/presets/mock_known_human.rs` | 缺少有效 `kh_editor` 的 `KnownHuman` 降级为普通 `Human`，不再写入 `h_*` 强人工 attestation；覆盖 `e2cc766` 这类 `promptCount=0` 却把 346 行 AI 误报成人工的场景 |
| 2.28 | Copilot terminal / bash post checkpoint 缺失时用 git status 兜底 | `git-ai/src/commands/checkpoint_agent/orchestrator.rs`、`git-ai/src/daemon.rs`、`git-ai/src/commands/git_ai_handlers.rs`、`git-ai/tests/integration/github_copilot_tools.rs` | `MissingPreSnapshot` / `SnapshotFailed` / `HookTimeout` / post-hook error 不再静默丢失 AI 改动，回退到 `git_status_fallback` 后继续发送 AI checkpoint；新增 `bash_post_checkpoint_resolved`、`checkpoint_request_send`、`checkpoint_request_started`、`checkpoint_request_finished` debug 事件 |
| 2.29 | Windows 安装器遇到 busy `git-ai.exe` 时退休旧文件并替换稳定路径 | `git-ai/install.ps1`、`git-ai/tests/windows_script_checks.rs` | 不再要求运行中的 exe 必须能打开独占写句柄；覆盖失败时先杀同安装目录进程树，再将旧 `git-ai.exe` 重命名为 `git-ai.exe.retired-*`，新二进制立即落回原路径；`GIT_AI_DEFER_IF_BUSY=1` passive update 下旧 daemon 仍运行也能完成安装 |
| 2.30 | Copilot checkpoint 兼容非 UTF-8 历史文本文件 | `git-ai/src/commands/checkpoint_agent/orchestrator.rs`、`git-ai/src/daemon.rs`、`git-ai/tests/integration/github_copilot_tools.rs` | `fs::read_to_string` 失败不再让显式 AI 编辑路径被 daemon 过滤；checkpoint 文件快照改为字节读取 + NUL 二进制排除 + UTF-8 lossy 文本化，daemon 端对 `content=None` 增加 workdir 兜底读取；覆盖 `op-return-exchange` 这类非 UTF-8 文本文件追加 AI 内容后落入 unknown 的场景，逻辑不依赖 `.java` 扩展名 |
| 2.31 | Windows 运行时健康收敛，补齐 2.29 安装器兜底的根因闭环 | `git-ai/src/commands/daemon.rs`、`git-ai/src/commands/git_ai_handlers.rs`、`git-ai/src/daemon.rs`、`git-ai/tests/daemon_mode.rs`、`git-ai/tests/integration/repos/test_repo.rs` | Windows named pipe 健康检查不再用 `.exists()` 误判离线；`--hook-input stdin` 默认 5 秒超时，宿主不关 stdin 时 checkpoint 子进程会自退出；失效 `active-runtime.json` 自动回退默认 runtime；Windows 测试 PATH 不再泄漏 `git-ai-test-home-*` 临时 bin；全程避免每命令全进程扫描，保护用户本机性能 |
| 2.32 | Windows Update Service 旧探活污染隔离 | `git-ai/src/integration/install_test_upload.rs`、`git-ai/src/integration/ide_mcp.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | git-ai Rust 探活增加 `installTestProducer=git-ai-rust`、`installerScript=git-ai-rust-install-test` 和对应 HTTP header；身份解析拒绝 `-BestEffort` / `-GitPath` 这类 PowerShell 参数形用户值，无效 env 不再遮住 MCP 配置，再回退 Git 邮箱 / IP；看板可据此只信任一手探活，隔离 `upload-install-test.ps1` 旧外部脚本直连产生的脏数据 |
| 2.33 | 同版本安装探活去重、after-commit 自动更新节流、debug JSONL 并发写锁 | `git-ai/src/integration/install_test_upload.rs`、`git-ai/src/commands/upgrade.rs`、`git-ai/src/diagnostics.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 同一版本 / 身份 / endpoint 的 `git-ai install success test` 成功后写入 marker，后续同版本自动安装只记录 skip 不再重复上传；提交后的自动更新尊重新鲜 no-update cache，只对已确认 pending update 继续安装；debug 日志写入用跨进程锁保护，避免并发进程把 JSONL 写坏；这是重复探活 / 排查噪音收敛，不直接补造缺失的 AI checkpoint |
| 2.34 | Copilot checkpoint 采集前置设置自愈 | `git-ai/src/mdm/utils.rs`、`git-ai/src/mdm/agents/vscode.rs`、`git-ai/src/mdm/agents/github_copilot.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | `install-hooks` 会确保 VS Code 用户 settings 中 `chat.useHooks=true`；当用户显式配置 `chat.hookFilesLocations` 时补齐 `~/.copilot/hooks`；清理指向不存在 `.git-ai\bin\git(.exe)` 或 `git-ai-test-home-*` 的坏 `git.path`，但不再无脑写入 Windows 已禁用的新 wrapper 路径；修复“hook 文件存在但没有 `checkpoint github-copilot`”的未来采集断点，历史无 checkpoint commit 仍保持 unknown |
| 2.35 | legacy `Human` checkpoint 明确人工新增不再落入 `unknownAdditions` | `git-ai/src/daemon/checkpoint.rs`、`git-ai/src/authorship/virtual_attribution.rs`、`git-ai/src/authorship/stats.rs`、`git-ai/tests/integration/prompt_across_commit.rs`、`git-ai/tests/integration/squash_merge.rs` | 普通 `git-ai checkpoint -- <file>` / 提交重放的 `Human` checkpoint 在没有 AI agent 和工具元数据时，为本次实际新增/改写行写入 commit author 的 `h_*` attestation；AI pre-edit baseline、bash pre-hook、弱 `KnownHuman` 降级仍不强归人工；同一新增行出现 AI 与 `h_*` 重叠时以 `h_*` 为准，工具维度 mixed 统计同步 clamp |
| 2.36 | Windows 直接安装器统一 launcher / current-exe / compatibility bin 版本 | `git-ai/install.ps1`、`git-ai/tests/windows_script_checks.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 手动安装指定 release 不再只覆盖 `.git-ai\bin\git-ai.exe`；安装器先更新权威 `.git-ai\launcher\git-ai.exe`，写 `.git-ai\current-exe` 指向 launcher，再同步 `.git-ai\bin\git-ai.exe` 兼容副本；避免 machine PATH 命中 launcher、user PATH 命中 bin 时出现一个显示 2.2.33、一个显示 2.2.32 的半安装状态 |
| 2.37 | Windows Update Service 停止默认外部 PowerShell 探活 | `windows-update-service/src/main.rs`、`windows-update-service/upload-install-test.ps1`、`windows-update-service/dist/ruijie-ai-update-service/upload-install-test.ps1`、`windows-update-service/README.md`、`windows-update-service/README.zh-CN.md`、`git-ai/docs/design-doc/git-ai看板方案.md` | 彻底切断 `author=-BestEffort` / `source=-GitPath` 来源：service 默认保留 git-ai Rust 内置探活，不再禁用后改由 `upload-install-test.ps1` 直连；legacy 外部脚本只在 `GIT_AI_SERVICE_RUN_LEGACY_INSTALL_TEST_UPLOAD=1` 时启用；兼容路径改用 hashtable 命名参数 splatting，脚本拒绝参数形 user/source 候选 |
| 2.38 | git-ai 安装器隔离 service skip 环境并发送自身 Rust 探活 | `git-ai/install.ps1`、`git-ai/tests/windows_script_checks.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | `install.ps1` 调 `install-hooks` 前临时移除父进程遗留的 `GIT_AI_SKIP_INSTALL_TEST_UPLOAD=1`，调用后恢复原值；不影响 `ruijie-ai-update-service` 正常安装 / 状态管理，但安装成功探活由 git-ai 自身 `installTestProducer=git-ai-rust` 发出；测试数据库环境仍不上传 |
| 2.39 | Windows Update Service 安装 git-ai 时 busy launcher 不再阻塞升级 | `windows-update-service/src/main.rs`、`windows-update-service/prepare-server-package.ps1`、`git-ai/install.ps1`、`git-ai/tests/windows_script_checks.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | service wrapper 的 pre-install cleanup 不再等待 `.git-ai\launcher\git-ai.exe` / `.git-ai\bin\git-ai.exe` 可独占写打开，busy 文件只记录非阻塞诊断并交给 git-ai 安装器 rename-retire 替换；远程旧安装脚本会被 patch 为优先退休旧 exe、失败后才杀进程兜底；Windows 直接安装 PATH 也优先 launcher，减少 launcher/bin 新旧入口混跑 |
| 2.40 | Windows Update Service 不再把已落盘成功误判为安装失败 / 卡住 | `git-ai/src/main.rs`、`windows-update-service/src/main.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 现场确认 2.2.36 是“二进制已替换成功但 service 安装事务失败”：`launcher\git-ai.exe` 和 `bin\git-ai.exe` 已返回 2.2.36，Rust 探活也可发送，但 `install-v2.2.36-*.log` 末尾显示 `install integrity failed: launcher version probe timed out`，`state.json` 仍停在 `last_result=installing` / `installed_semver=2.2.33`，服务为 Stopped。根因是安装末尾用 `git-ai version` 探测新 exe，版本命令仍会写 debug 日志并可能被日志锁 / 卡住进程拖住；wrapper 超时后只 Kill 未 Wait，残留 `git-ai --version/version`；service 父进程还用 `Command::output()` 捕获 PowerShell stdout/stderr，若子进程继承管道句柄，外层 `install.ps1` 会表现为不返回。修复：`git-ai` 的 help/version 等快速元数据命令不再写 debug 日志；service 所有版本探针改用 `--version`、注入 `GIT_AI_DEBUG=0` / `GIT_AI_DEBUG_STDERR=0`、超时后 Kill+Wait；Rust 侧版本检测也有 8s 超时；service 启动 PowerShell 安装脚本时不再捕获 stdout/stderr 管道，而依赖 wrapper transcript 写安装日志并等待进程退出，从根上避免“git-ai 已能用但 service 仍 installing/假失败/卡住”。 |
| 2.41 | 安装探活作者不再允许使用安装器/工具标签 | `git-ai/src/integration/install_test_upload.rs`、`windows-update-service/upload-install-test.ps1`、`windows-update-service/dist/ruijie-ai-update-service/upload-install-test.ps1`、`git-ai/docs/design-doc/git-ai看板方案.md` | 明确 `git-ai-install` / `install-success` 只是安装成功探活维度；真正异常是 `author=git-ai installer`。Rust 内置探活拒绝 `git-ai installer`、`git-ai-install`、`install-success`、`upload-install-test.ps1`、`windows-update-service-legacy` 等 reserved identity 并继续回退真实 MCP / Git 邮箱 / IP；legacy PowerShell 探活脚本移除 `git-ai installer` 兜底，最差只写 `unknown-install-user`，避免工具名再进入看板作者。 |
| 2.42 | Copilot native / CLI hook 在工具输入输出无路径时用 `dirtyFiles` key 兜底 | `git-ai/src/commands/checkpoint_agent/presets/github_copilot/mod.rs`、`git-ai/src/commands/checkpoint_agent/presets/github_copilot/ide.rs`、`git-ai/src/commands/checkpoint_agent/presets/github_copilot/cli.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 当 `tool_input` / `tool_response` 只有文本片段、执行结果或脱敏内容，没有 `file_path/path/files` 时，使用当前 hook 顶层 `dirtyFiles` / `dirty_files` 的 key 作为文件路径兜底；修复 `rj-ltc-contract-web`、`ai-cr-manage-service`、`op-api` 这类 AI 生成文件大量掉到 `unknown` 的路径缺失问题 |
| 2.43 | recent `KnownHuman` 保存不再覆盖已有 AI 行 | `git-ai/src/daemon/checkpoint.rs`、`git-ai/tests/integration/pending_ai_edit_suppression.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | `AI Edited` 后 IDE 保存 / 文件监听触发的 `known_human` 是正常竞态；1 秒内仍拒绝，30 秒内 recent AI save 只对真实 changed lines 写 `h_*`，已有 AI 行保持 AI，用户后续手工补充的行仍能计入人工 |
| 2.44 | commit 后自动更新先让本次 commit 上传落地 | `git-ai/src/commands/git_handlers.rs`、`git-ai/src/integration/upload_stats.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | `git commit` 成功后先 spawn fallback `upload-stats --wait-for-authorship-note-ms 5000 --skip-if-already-uploaded --acquire-activity-lock-before-stats`，再调度后台自更新；自动更新功能不受影响，但不会先重启 / 替换 runtime 导致远程看板看不到本次 commit |
| 2.45 | 自动上传身份解析支持工作区 `.mcp.json` / `mcpServers` | `git-ai/src/integration/ide_mcp.rs`、`git-ai/docs/design-doc/git-ai看板方案.md`、`git-ai/docs/design-doc/获取x-user-id实现方案.md` | 从仓库根向父级工作区查找 `.mcp.json` / `.vscode/mcp.json`，并同时解析顶层 `servers` 与 `mcpServers`；修复徐建丰 2026-06-25 这类 HTTP 200 已上传但 `hasUserId=false/userIdSource=missing`，导致看板按人/部门维度查不到自动提交记录的问题 |
| 2.46 | Windows launcher git proxy 与 git-ai 入口强制同步 | `git-ai/install.ps1`、`git-ai/src/commands/install_hooks.rs`、`git-ai/tests/windows_script_checks.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 安装时同时刷新 `.git-ai\launcher\git.exe` 与 `.git-ai\bin\git.exe`，PATH 继续优先 launcher；`git-ai install-hooks` 通过新版 `git-ai.exe` 运行时也会覆盖同目录旧 `git.exe`。修复黄芳 2026-06-26 这类 `git-ai.exe` 已是 2.2.46、但 commit 仍由旧 `launcher\git.exe` 2.2.44 拦截，导致没有 post-commit/upload 事件的问题 |
| 2.47 | Copilot mixed workspace 漏采与 replay Human 误报人工修复 | `git-ai/src/commands/checkpoint_agent/orchestrator.rs`、`git-ai/src/daemon/checkpoint.rs`、`git-ai/tests/integration/github_copilot_tools.rs`、`git-ai/tests/integration/post_commit_unit.rs`、`git-ai/tests/integration/repos/test_repo.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | Copilot 在上层工作区一次 payload 混入非 Git 仓库路径时，不再让该路径拖垮同批子仓库 AI checkpoint；synthetic replay / backfill Human checkpoint 保留 `git_ai_replay_checkpoint=true` 等弱证据标记，post-commit 不再把这类快照误判为 plain manual Human 并写入 `h_*`。修复锦召 `ai-rag-doc` / `ai-rag-service-python` 这类“目标提交没有 AI checkpoint、又被 replay Human 误补成人工”的本地坏数链路 |
| 2.48 | 观察到新 HEAD/base 后补偿漏 post-commit 提交 | `git-ai/src/git/sync_authorship.rs`、`git-ai/src/daemon.rs`、`git-ai/tests/integration/post_commit_unit.rs`、`git-ai/docs/design-doc/git-ai看板方案.md` | 后续 checkpoint 的 `baseCommit` 或 IDE 触发的普通 Git 查询证明 HEAD 已推进时，如果目标 commit 缺 authorship note 且父 working log 仍有 checkpoint / INITIAL 证据，则复用 post-commit 收口生成 note 并进入上传；无本地证据的 raw commit 仍保持 unknown。修复龙宇 2026-06-27 `rj-ltc-web` 18:05 / 18:42 这类 commit 绕过 post-commit 且没有 push 兜底时看板漏传的问题 |

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
| 4.5 | 修复 `git_ai_tool_stats` 主键类型与 `ASSIGN_ID` 雪花 long 不一致 | `ai-cr-manage-service` 发布后，连续上传不会再因为 `INT/Integer` 截断而触发 `Duplicate entry ... git_ai_tool_stats.PRIMARY`；数据库列需按 `docs/design-doc/git_ai_tool_stats_primary_key_fix_2026-05-04.sql` 手工改为 `BIGINT` |

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
| **git-ai 原生自动上传失败 / 超时** | `post_commit` 里的原生上传只做 best-effort；payload 组装失败、网络错误、非 2xx 响应都只记 debug 日志并跳过，不中断 commit | 即时上传是增强能力，不能为了网络成功率牺牲 commit 成功率 |
| **git-ai background service 启动被锁阻塞**（旧版常见：`daemon startup blocked: lock held at ...daemon.lock`；新版恢复失败时可能带 `failed to recover unhealthy daemon pid ... taskkill ... 拒绝访问`） | 这是本机 daemon 连接 / 启动问题，不是远程 API 上传失败。报错 1 表示旧版看到锁后直接失败；报错 2 表示新版已经识别出旧 daemon 不健康并尝试 `taskkill`，但 Windows 权限拒绝结束旧进程。最快恢复仍是执行 `git-ai bg restart --hard`；当前实现不会再在 `taskkill` 被拒绝时自动切换 replacement runtime，而是保留 blocked startup 错误，避免继续制造新的 `bg run` 进程去争用全局上传锁 | `daemon.lock` 是进程级互斥锁，不能靠手动删除文件安全恢复；Windows 权限拒绝时也不能继续靠“换 runtime”掩盖问题，否则会把一个坏 daemon 扩散成多套 daemon runtime |
| **全局 `core.hooksPath` 指向不存在目录** | `install-hooks` 必须自动检测并清理失效的历史 hooksPath；现场排查时执行 `git config --global --show-origin --get core.hooksPath`，若路径不存在，先清理再重新安装 hooks | Git 会按 `core.hooksPath` 查找 hook；路径不存在时，commit 后不会进入 post-commit，表现为 commit 没 note、`debug.jsonl` 没有该 commit |
| **安装了新版但旧 daemon 还在跑** | 替换 `%USERPROFILE%\.git-ai\bin\git-ai.exe` / `git.exe` 后必须确认旧 `git-ai.exe bg run` 已退出；`install-hooks` 在 Windows 下会按安装目录兜底清理残留后台进程，再启动新 daemon | Windows 上磁盘文件更新不会替换旧内存镜像；如果旧 daemon 继续接收 Trace2，真实 commit 仍会走旧逻辑 |
| **upload-ai-stats.ps1 上传失败**（API 不可达） | 一次批量请求失败时整批标记失败；若服务端返回 `results[]`，则按 commit 维度展示"N 成功, M 失败" | 降低请求次数，同时保留按 commit 追踪失败的能力 |
| **Code Review 时 git-ai 未安装** | 步骤 8.3 检测到 `git-ai --version` 失败后，直接跳到步骤 9，审查报告正常生成但没有 AI 统计表格 | 审查报告的核心价值是代码质量问题，AI 统计是附加信息 |
| **某个 commit 没有 AI authorship note** | 仍然调用 `git-ai stats <sha> --json`，但将 `hasAuthorshipNote=false` 且把该 commit 归到 `unknownAdditions` 视图 | 最新 `stats` 已能表达“没有归因 note，但有新增行”的情况，直接跳过会丢失有效数据 |
| **后端报 `Field 'model' doesn't have a default value`** | 客户端三条上传链路都必须补齐 `model`，无法解析时写 `unknown` | 后端表字段无默认值，客户端要提交稳定 schema，不能依赖数据库默认值 |
| **后端无法解析 timestamp / 看板时间差 8 小时** | 上传 payload 的 commit 时间先解析原始时区，再转换为北京时间 `yyyy-MM-dd HH:mm:ss` | Java `Date` 不接受 RFC3339 纳秒格式；团队看板按北京时间展示，不能只截 `%aI` 字符串 |
| **API Key 泄露** | API Key 只存在本机环境变量或 CI Secret，不进入仓库文件 | 密钥不进 git，即使 `.specify/` 被提交也不含敏感信息 |
| **网络超时** | 上传请求设置 10 秒 timeout | 防止长时间挂起，影响开发体验 |
| **代码内容泄露** | 只上传统计数据（行数、比例、工具名），**从不上传代码内容** | 隐私第一：统计数据足够做管理决策，不需要源代码 |
| **重复上传同一个 commit** | 远端 API / 服务端按 `commitSha` 做幂等过滤 | 幂等设计：多次上传不会产生重复记录 |

---

## 七、配置示例——拿去就能用

### git-ai 原生自动上传（当前实现）

如果你希望当前机器在每次 commit 成功后立即尝试上传，请至少配置下面这几项：

```powershell
# 默认已开启；如需显式配置，使用 true / false
[Environment]::SetEnvironmentVariable("GIT_AI_AUTO_UPLOAD_AI_STATS", "true", "User")
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_API_KEY", "your-personal-key", "User")

# 二选一：完整 URL，或 endpoint + path
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_URL", "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats", "User")
# [Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_ENDPOINT", "https://service-gw.ruijie.com.cn", "User")
# [Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_PATH", "/api/ai-cr-manage-service/api/public/upload/ai-stats", "User")

# 可选：如果不配，git-ai 会继续尝试从 MCP 配置中解析 X-USER-ID
[Environment]::SetEnvironmentVariable("GIT_AI_REPORT_REMOTE_USER_ID", "your-user-id", "User")

# 可选：排查上传时打开 debug 日志
[Environment]::SetEnvironmentVariable("GIT_AI_DEBUG", "1", "User")

$env:GIT_AI_DEBUG = "1" 临时排查
```

**当前行为要点：**
- `GIT_AI_AUTO_UPLOAD_AI_STATS` 默认已开启；如需关闭，设置为 `false`。如需显式覆盖，请使用 `true` / `false`，不要使用 `1` / `0`。
- `GIT_AI_REPORT_REMOTE_URL`、`GIT_AI_REPORT_REMOTE_ENDPOINT`、`GIT_AI_REPORT_REMOTE_PATH` 都没配时，会回退到内置默认地址。
- `GIT_AI_REPORT_REMOTE_USER_ID` 没配时，普通统计上传会复用 `resolve_x_user_id(...)` 从 MCP 配置中继续找 `X-USER-ID`；安装成功测试上传还会按 MCP / 环境用户 ID、Git 邮箱、IP 地址兜底，不能因为 MCP 用户 ID 缺失跳过，也不能伪造测试用户。
- 正常 commit 链路会在 `post_commit` 写出 authorship note 并算出 stats 后即时上传；若大提交在 post-commit 快路径跳过昂贵 stats，上传任务会按允许补算标记后台补算后再上传。
- 如果 commit 没有 `refs/notes/ai` authorship note，自动 / 手动上传都不再直接跳过；上传 `hasAuthorshipNote=false` 的 metadata-only payload，并把新增行计入 `unknownAdditions`。
- 所有上传 payload 都必须补齐后端必填字段，尤其是 `model` 缺失时填 `unknown`；commit 时间必须先解析原始时区，再转换为北京时间 `yyyy-MM-dd HH:mm:ss`。
- `GIT_AI_DEBUG` 打开后，除了 stderr，还会写入本地 `~/.git-ai/logs/debug.jsonl`，用于排查“这次 checkpoint 到底被判成 AI 还是人工”以及“本次统计为什么没有上传到远程服务”。

### 当前项目默认安装 / 更新来源

当前这个定制版 Spec Kit 已经把 git-ai 的默认安装 / 更新目标固定为：

```text
rj-gaoang/git-ai @ latest
```

也就是说，正常执行 `specify init` 或 `specify init --here --force` 时，post-init 会自动采用下面这组默认值，不需要成员再手工配置：

```powershell
GIT_AI_INSTALLER_URL = https://raw.githubusercontent.com/rj-gaoang/git-ai/main/install.ps1
GIT_AI_GITHUB_REPO = rj-gaoang/git-ai
GIT_AI_RELEASE_TAG = latest
```

对应 Bash / zsh：

```bash
GIT_AI_INSTALLER_URL=https://raw.githubusercontent.com/rj-gaoang/git-ai/main/install.sh
GIT_AI_GITHUB_REPO=rj-gaoang/git-ai
GIT_AI_RELEASE_TAG=latest
```

这里有一个容易误解的点：**默认值里的 latest 只控制最终安装的二进制 release asset，installer 脚本本身默认来自 `main` 分支源码地址。** 所以：

- 只有在 GitHub `latest` 已经正确指向目标稳定版、且同名 asset 本身也真的是该版本二进制时，Spec Kit 下次 force-update 才会拉到正确的 latest release binary。
- 如果 `latest` 还停在旧 tag、目标 release 仍是 draft / prerelease，或者同名 asset 内嵌版本号错误，Spec Kit 仍会跟着拉到错误 binary；这时要先修正 GitHub release，再谈“自动更新为什么没生效”。
- 如果你未来想临时切到别的仓库、别的 tag，或者验证本地 binary，仍然可以覆盖这些默认值。

### 如果未来要临时覆盖默认来源

如果后续某一次验证不想跟随 `rj-gaoang/git-ai@latest`，而是临时换仓库 / 换 tag / 换本地 binary，可以用下面这几种覆盖方式：

1. `GIT_AI_INSTALLER_URL`：直接切 installer 下载地址。
2. `GIT_AI_GITHUB_REPO`：切换到别的 GitHub repo。
3. `GIT_AI_RELEASE_TAG`：把 latest 改成固定 tag。
4. `GIT_AI_LOCAL_BINARY`：完全跳过下载，直接使用本地 binary。

### 如果要修正 GitHub Release 的 latest 指向 / 资产版本

这一步不是改本机配置，而是修正发布侧状态。当前仓库的 `.github/workflows/release.yml` 已经内建了这套语义：只有在手动运行 `Release Build` 并把 `release_production=true` 时，workflow 才会产出稳定 tag、把 `prerelease=false`，并显式设置 `make_latest=true`；否则它只会生成 `vX.Y.Z-next-<sha>` 的 `next` 预发布版本，不会改 GitHub 的 latest 指向。

建议按下面顺序处理：

1. 先在准备发布的目标 commit 上确认版本真的是 `2.1.11`。最低核对项是 `Cargo.toml` 里的 `version = "2.1.11"`，以及从该 commit 构建出来的 Windows 二进制执行 `--version` 时也返回 `2.1.11`。如果这里还是 `2.1.10`，先不要发 release。
2. 核对线上是否已经存在错误的 `v2.1.11`。如果 release/tag 本身就挂在错误 commit 上，最稳妥的做法是先删除错误 release 和错误 tag，再从正确 commit 重跑 production release；如果 tag 正确但只是 asset 错了，也可以只删除错误 asset 后重新上传正确构建产物。
3. 在 GitHub Actions 手动运行 `Release Build`，并明确设置 `dry_run=false`、`release_production=true`。按当前 workflow 逻辑，这会生成 `tag_name=v2.1.11`、`channel=latest`、`prerelease=false`、`make_latest=true` 的正式发布，而不是 `next` 预发布。
4. 发布完成后立即验证 GitHub latest 是否已经切对。建议至少执行两条检查：`gh api repos/rj-gaoang/git-ai/releases/latest --jq '.tag_name'` 应返回 `v2.1.11`；`curl -I -L https://github.com/rj-gaoang/git-ai/releases/latest/download/git-ai-windows-x64.exe` 的 302 跳转目标里应包含 `/releases/download/v2.1.11/`。
5. 再把实际下载到的 Windows asset 跑一遍 `--version`。只有当 latest 的 302 跳转目标和二进制自报版本都等于 `2.1.11`，才算“外部自动更新链路已经真正修正完成”。否则说明你修掉的只是 release 元数据，asset 本体还没对上。
6. 如果你只是临时想绕过错误的 latest，而不是马上修复发布侧，可以先把安装链路显式固定到 `GIT_AI_RELEASE_TAG=v2.1.11`，或者直接使用 `GIT_AI_LOCAL_BINARY`。这只能作为短期止血，不能替代对 GitHub latest 的正式修复。

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
- 安装或更新 git-ai（当前默认从 `https://raw.githubusercontent.com/rj-gaoang/git-ai/main/install.ps1` 下载修正版 installer，并自动补齐 `GIT_AI_GITHUB_REPO=rj-gaoang/git-ai` 与 `GIT_AI_RELEASE_TAG=latest`；如需临时覆盖，仍可改 `GIT_AI_INSTALLER_URL` / `GIT_AI_GITHUB_REPO` / `GIT_AI_RELEASE_TAG`）
- 刷新 `git-ai install-hooks` 集成配置

它**不会**默认写入 `GIT_AI_AUTO_UPLOAD_AI_STATS`、`GIT_AI_REPORT_REMOTE_URL`、`GIT_AI_REPORT_REMOTE_ENDPOINT`、`GIT_AI_REPORT_REMOTE_PATH`、`GIT_AI_REPORT_REMOTE_API_KEY` 或 `GIT_AI_REPORT_REMOTE_USER_ID`。这些值目前仍然建议由团队脚本追加，或由成员自行在本机环境变量里配置。`git-ai` 原生自动上传也复用这一套环境变量，不额外引入 `git-ai config` 配置键。

不过，如果这些 URL 相关环境变量都未设置，Windows 下的上传脚本会回退到内置默认地址 `https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats`。

不过，Windows 下的上传脚本和 `git-ai` 原生上传现在都会在 `GIT_AI_REPORT_REMOTE_USER_ID` 未设置时，继续尝试从本机 VS Code / IDEA 的 MCP 配置中读取 `X-USER-ID`，以便和 Code Review MCP 配置保持一致。

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
| 安装 / 更新 Speckit 时自动安装 / 更新 git-ai | `post-init` 脚本 + 修改 `specify init` 源码以 force-update 语义执行 post-init + installer 运行时 repo 覆盖 | 新建 PowerShell/Bash 模板源 + 同步副本 + 修改 1 个 Python 入口函数 + 修改 2 个 installer | 真正覆盖 `specify init` / `specify init --here --force`，并允许切到自定义 GitHub 仓库 |
| commit 后即时上传 AI 统计 | `git-ai` 原生 `post_commit` 上传 | 新增 `upload_stats.rs` + 接入 `post_commit` + `auto_upload_ai_stats` feature flag | 满足“生成统计结果就上传”的时效性要求，同时保持 best-effort，不阻塞 commit |
| prompt 正文默认落库 | `prompt_storage` 默认切到 `notes` | 修改 `git-ai/src/config.rs` 默认值和非法值回退 | 让新用户无需额外配置，也能让 `git_ai_prompt_stats.prompt_text` 从 note 中稳定提取 |
| 本地排查 AI/人工归因和上传失败 | 默认写本地 JSONL 日志，必要时用 `GIT_AI_DEBUG=false` 关闭 | 新增并强化 `git-ai/src/diagnostics.rs`，修改 `git-ai/src/commands/checkpoint.rs`、`git-ai/src/authorship/post_commit.rs`、`git-ai/src/integration/upload_stats.rs`，新增 `git-ai/docs/design-doc/git-ai环境变量与配置说明.md` | stderr 不可见时，仍能在本机确认 checkpoint 的 AI / 人工判断结果、stats 是否计算、payload 是否生成、HTTP 是否发出、远程返回什么状态；日志带时间并有 2GB 上限治理，不影响正常功能 |
| 开发者主动上传 AI 统计 | `upload-ai-stats.ps1` 命令 | 新建 1 个 PowerShell 脚本 | 开发者掌控上传时机，不阻塞 commit，并通过一次批量请求上传多个 commit |
| Code Review 自动上传 | 扩展 Code Review Agent 步骤 8 | 修改 1 个 agent prompt + 1 个报告模板 | 无需 reviewer 额外操作，数据自动附带 |

**三个核心设计原则：**

1. **职责分层清晰** ——Speckit 负责安装、批量上传和 Code Review 展示；git-ai 负责 commit 主链路内的即时统计与可选原生上传。
2. **不阻塞开发流程** ——原生上传挂在 `post_commit` 里，但通过 feature flag 控制、超时控制和 best-effort 失败策略保护，commit 永远优先。
3. **降级友好** ——git-ai 未安装、stats 被跳过、API 不可达、MCP 未配置时都能优雅降级，不影响 Speckit 核心流程。

**最终效果：**
- 新成员：clone → `specify init` 自动触发 post-init（force-update 语义）→ 自动安装 git-ai 到目标最新版本 → 正常开发 → AI 使用自动记录
- 开发者：默认每次 commit 成功后立即尝试上传；如果需要显式补传当前或指定 commit，可执行 `git-ai upload-stats [<commit>...]` 或别名 `git-ai upload-ai-stats [<commit>...]`；如果需要历史范围回补，仍可执行 `upload-ai-stats.ps1`；如需关闭即时上传，可设置 `GIT_AI_AUTO_UPLOAD_AI_STATS=false`
- Reviewer：Code Review → 自动附带 AI 统计表格，并作为最后一道补传兜底 → 无需额外操作

---

## 变更日志

### 2026-06-14：Windows 安装器 busy exe 替换闭环

**变更原因：**

`v2.2.30` Windows 安装现场出现反复打印 `Waiting for file to be available: C:\Users\admin\.git-ai\bin\git-ai.exe` 和 `Stopping lingering git-ai processes: ...` 的问题。用户手工执行 `taskkill /F /IM git-ai.exe /T` 后，输出显示真正被终止的是一批子进程，而安装器日志中的多个父 PID 随后又提示“没有此任务的实例在运行”。这说明问题不只是“某个进程没杀掉”，而是旧安装器的两个假设不成立：第一，运行中的 Windows exe 不能用独占写句柄打开，但仍可以先重命名到同目录退休路径，再把新 exe 放回原稳定路径；第二，只对 `Win32_Process` 枚举到的 PID 执行 `Stop-Process`，无法保证清掉 hook / checkpoint / daemon 的完整子进程树，真实占用者可能继续持有旧 `git-ai.exe` 镜像。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/install.ps1` | 新增 `Install-BinaryWithRenameFallback` | 替换 `git-ai.exe` 时，直接覆盖失败后不再无限等待可写句柄，而是退休旧 exe、恢复稳定路径到新 exe，避免安装卡在运行中 binary 上 |
| `git-ai/install.ps1` | 新增 `Register-DeleteOnReboot` / `Remove-OrScheduleDelete` | 旧 retired exe 能立即删除就删除；仍被进程持有时登记为下次重启删除，避免安装目录长期堆积旧 binary |
| `git-ai/install.ps1` | 新增 `Stop-ProcessTree`，`Stop-GitAiManagedProcesses` 改用 `taskkill.exe /F /T /PID` | 清理同安装目录进程时杀完整进程树，避免只杀父进程留下真正锁文件的 hook / checkpoint 子进程 |
| `git-ai/install.ps1` | legacy `git.exe` wrapper 禁用改为优先重命名，失败后才等待 | 老用户从旧 wrapper 迁移时，不再因为 wrapper 正在运行就提前进入长时间等待 |
| `git-ai/tests/windows_script_checks.rs` | 新增 `windows_install_script_passive_update_retires_busy_exe` | 隔离 HOME 下真实跑 `install.ps1`：先安装、启动旧 `git-ai.exe bg run` 占用 exe，再用 `GIT_AI_DEFER_IF_BUSY=1` 重装，验证安装成功且输出 retired 旧 exe |
| `git-ai/tests/windows_script_checks.rs` | 新增静态约束测试 | 锁定主流程必须走 busy-binary rename fallback，并使用 `taskkill /T` 清理进程树，防止后续回退为直接覆盖 / 单 PID `Stop-Process` |

**验证结论：**

- `cargo fmt --check` 通过。
- `install.ps1` PowerShell parser 检查通过。
- `cargo test --test windows_script_checks -- --nocapture` 通过，13 个测试覆盖新用户安装、老 wrapper 禁用、daemon 运行中重装、passive auto-update 下 busy `git-ai.exe` 仍运行但安装成功。
- 本次修复没有通过手工清理当前机器安装目录来规避问题；验证均在测试隔离 HOME 中执行，用来保证其他用户安装 / 升级遇到相同进程占用时也能完成替换。

### 2026-06-14：张一峰 op-api 弱 KnownHuman 误归人工与 bash checkpoint 漏归因修复

**变更原因：**

`logs/zyf-20260614.jsonl` 暴露出两类独立归因问题。`2026-06-01 15:19:37` 左右的 `op-api` 提交 `e2cc766` 中，实际 346 行为 AI 生成，但日志显示 `promptCount=0`、`sessionCount=0`、`aiAdditions=0`、`humanAdditions=346`、`unknownAdditions=7`。这是弱 `KnownHuman` 自动保存事件在没有 AI checkpoint / prompt 证据时被强行写成 `h_*` 人工 attestation 导致的误判。`2026-06-11 13:58:10` 左右的 `op-api` 提交 `991aca4` 中，`src/main/java/com/ruijie/op/api/controller/JobTask.java` 实际为 AI 生成，但只识别出约 150 行 AI，其余落到 unknown。这里不是服务端映射错误，而是 terminal / bash 类 Copilot 调用缺少 pre snapshot 或 post checkpoint 失败后，旧链路没有把实际变更文件补送给 daemon，导致部分新增行没有归因记录。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/daemon/checkpoint.rs` | 新增 `effective_checkpoint_kind(...)`，缺少有效 `kh_editor` 的 `KnownHuman` 降级为普通 `Human` | 弱 IDE 自动保存不再生成 `h_*` 强人工归因，避免把无 AI checkpoint 的新增代码误报成人工 |
| `git-ai/src/commands/checkpoint_agent/presets/mock_known_human.rs` | 测试 mock 的真实 KnownHuman 补齐 `kh_editor` / editor version 元数据 | 保留“明确人工编辑器事件”仍可归人工的测试语义 |
| `git-ai/src/authorship/stats.rs` | 新增 `weak_known_human_without_ai_checkpoint_does_not_claim_human_additions` 回归测试 | 锁定 `e2cc766` 这类“弱 KnownHuman + 无 AI checkpoint”不能产生人工新增行 |
| `git-ai/src/commands/checkpoint_agent/orchestrator.rs` | `PostBashCall` 在 `MissingPreSnapshot` / `SnapshotFailed` / `HookTimeout` / error 时调用 `git_status_fallback` | Copilot terminal / bash 修改文件后，即使前置快照缺失也能把实际变更文件作为 AI checkpoint 发送 |
| `git-ai/src/commands/checkpoint_agent/orchestrator.rs` | 新增 `bash_post_checkpoint_resolved` debug 事件 | 能直接看到 bash post hook 的 action、fallback reason、检测文件数和最终发送文件数 |
| `git-ai/src/daemon.rs`、`git-ai/src/commands/git_ai_handlers.rs` | 新增 `checkpoint_request_send`、`checkpoint_request_started`、`checkpoint_request_finished` debug 事件 | 排查时可区分“客户端没发出”、“daemon 没收到”和“daemon 处理失败” |
| `git-ai/tests/integration/github_copilot_tools.rs` | 新增 `test_run_in_terminal_missing_pre_snapshot_uses_git_status_fallback` | 覆盖缺少 bash pre snapshot 时仍能把 terminal 生成文件归为 AI |

**验证结论：**

- `cargo fmt` 通过。
- `cargo check -q` 通过。
- `cargo test -q weak_known_human_without_ai_checkpoint_does_not_claim_human_additions --lib` 通过。
- `cargo test -q --test integration test_run_in_terminal_missing_pre_snapshot_uses_git_status_fallback` 通过。
- Windows 下直接跑未限定 harness 的过滤测试时，Cargo 还会尝试启动 `commit_tree_update_ref` 测试二进制，可能报 OS error 740 需要提权；该问题与本次归因逻辑无关，针对性使用 `--lib` / `--test integration` 可正常验证。

### 2026-06-15：op-return-exchange 非 UTF-8 文本文件漏归因修复

**变更原因：**

`D:\rj-op\op-return-exchange` 最新提交 `cf060485afcfdacc4d0c85b0ee9902e08798fa3e` 中，用户点名的 `src/test/java/com/ruijie/opreturnexchange/OpReturnExchangeApplicationTests.java`、`src/test/java/com/ruijie/opreturnexchange/SplitTests.java`、`src/test/java/com/ruijie/opreturnexchange/service/MailTest.java` 每个都新增了 `copilotAddedSimpleTest2` 的 5 行测试方法，但最终 authorship note 没有这些文件，`git-ai stats HEAD --json` 显示 `unknown_additions=75`。`debug.jsonl` 里同一 trace 的 `bash_post_checkpoint_resolved` 已检测到 90 个文件，`daemon_checkpoint_request_resolved_files` 却只解析出 79 个文件，差集里的 11 个 Java 源文件每个新增 5 行，正好解释 55 行漏归因；再叠加未归因的 class / workspace 等非源码改动形成最终 unknown。对用户点名的 3 个文件做严格 UTF-8 读取均失败，说明它们是含非 UTF-8 字节的历史文本文件。虽然现场样本是 Java，但失败条件来自通用文本读取链路，任何扩展名的非 UTF-8 文本文件都可能触发。

根因不是 Copilot 没有识别文件，而是 `build_checkpoint_files(...)` 用 `fs::read_to_string(path).ok()` 读取整文件；遇到非 UTF-8 字节时 content 变成 `None`。daemon 侧旧实现又只把 `file.content=Some(...)` 的路径加入 `resolved.files` 和 `dirty_files`，导致“路径已发现、但内容读取失败”的显式 AI 编辑被静默过滤，post-commit 只能把这些新增行记为 unknown。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/commands/checkpoint_agent/orchestrator.rs` | 新增 `read_checkpoint_file_content(...)`，从 `read_to_string` 改为 `fs::read` 字节读取；包含 NUL 字节时视为二进制跳过，否则用 `String::from_utf8_lossy` 生成文本快照 | 非 UTF-8 但仍是文本的 Java / legacy 文件可以进入 checkpoint，不再因为 UTF-8 解码失败变成 `content=None` |
| `git-ai/src/daemon.rs` | `resolve_checkpoint_request(...)` 在请求 content 缺失时调用 `read_checkpoint_content_from_workdir(...)` 兜底读取工作区文件 | 旧 hook / 其他插件即使发来 `content=None`，daemon 也会尽力保留显式路径，避免请求解析阶段再次静默丢文件 |
| `git-ai/tests/integration/github_copilot_tools.rs` | 新增 `test_run_in_terminal_non_utf8_existing_text_files_are_attributed` | 复现“既有非 UTF-8 `.java` 和 `.properties` 文本文件 + Copilot terminal fallback 追加内容”，验证提交后 `unknown_additions=0`、`ai_additions=6` |
| `git-ai/tests/integration/performance.rs` | `FeatureFlags` 测试构造补 `..FeatureFlags::default()` | 避免新增 feature flag 字段后 integration harness 编译被无关测试字面量阻断 |

**验证结论：**

- `cargo fmt` 通过。
- `cargo test --test integration github_copilot_tools::test_run_in_terminal_non_utf8_existing_text_files_are_attributed -- --nocapture` 通过。
- `cargo test --test integration github_copilot_tools::test_run_in_terminal -- --nocapture` 通过，覆盖 no changes、正常 bash checkpoint、missing pre snapshot fallback、非 UTF-8 既有文件四个同组场景。

### 2026-06-15：Windows busy exe 方案缺口与运行时健康收敛

**变更原因：**

`2.29` 的安装器 retired binary 方案解决的是“Windows 上运行中的 `git-ai.exe` 被占用时，新二进制仍能落回稳定路径”。这个方向没有错，但它存在明显缺口：它没有减少 `git-ai.exe` 被占用的来源，也不能处理 replacement runtime 失效后客户端继续路由到旧 runtime 的问题。现场最新提交的上传日志表明，真正的 HTTP 上传约 108ms，post-commit 到 payload/HTTP 成功约 2.7 秒；慢的是提交后约 74 秒才进入 `post_commit_started`，说明问题发生在事件路由 / daemon 健康 / 残留 checkpoint 进程阶段，而不是服务端上传慢。

本轮确认的根因链路是四段叠加：Windows named pipe 路径 `\\.\pipe\...` 不是普通文件，旧 `daemon_is_up(...)` 先做 `.exists()` 会把健康 daemon 误判为离线；`checkpoint ... --hook-input stdin` 遇到宿主不关闭 stdin 时会无限等待，残留进程继续持有旧 exe 镜像；`active-runtime.json` 指向的 replacement runtime 失效后没有自动回退默认 runtime；Windows 测试 harness 没有清理 `git-ai-test-home-*` 临时 bin，可能把测试 shim 泄漏到真实 PATH。`2.29` 因此是必要兜底，但不是彻底方案。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/commands/daemon.rs` | Windows 下 `daemon_is_up(...)` 不再对 named pipe 路径做 `.exists()`，直接用 100ms socket/pipe connect 探测 | 正常运行的 daemon 不会被误判为离线，避免重复启动 / replacement runtime 扩散 |
| `git-ai/src/commands/git_ai_handlers.rs` | `--hook-input stdin` 改为带超时读取，默认 `GIT_AI_HOOK_STDIN_TIMEOUT_MS=5000`，超时后以 0 退出并跳过 checkpoint | IDE / Agent 宿主异常不关闭 stdin 时，checkpoint 子进程不会无限挂住，也不会长期持有旧 exe |
| `git-ai/src/daemon.rs` | `active_runtime_config(...)` 读取 `active-runtime.json` 后，只有 control/trace 可连接或 runtime lock 仍被持有时才保留；否则删除元数据并回退默认 runtime | replacement runtime 失效后自动收敛到稳定路径，不需要用户手工清理 `active-runtime.json` |
| `git-ai/tests/daemon_mode.rs` | 新增 `checkpoint_hook_stdin_times_out_when_host_keeps_pipe_open`，并把测试 PATH 清理改为 Windows/Unix 通用 | 回归覆盖 stdin 未关闭导致 checkpoint 残留的进程堆积根因；daemon 专项测试不再泄漏临时 git-ai shim |
| `git-ai/tests/integration/repos/test_repo.rs` | Windows 测试环境同样剔除 `git-ai-test-home-*`、含 `git-ai.exe` 或 git-ai wrapper 的 PATH 目录 | 集成测试不会通过 PATH 命中真实安装版或临时安装版 git-ai，降低测试污染真实运行时的风险 |
| `git-ai/src/git/test_utils/mod.rs` | 给 `TmpRepo` 补 `git_command(...)` 测试工具包装 | 恢复当前库单测编译通道，避免无关测试工具 API 缺口阻塞验证 |

**性能边界：**

- 不在普通命令路径做全系统进程扫描，也不按进程名高频清理用户机器。
- active runtime 只在存在 `active-runtime.json` 时做轻量 socket/lock 探测。
- hook stdin 超时只作用于显式 `--hook-input stdin` 的 hook 调用。
- 进程树清理仍保留在安装 / 重启等低频维护路径，作为兜底，而不是常态控制面。

**验证结论：**

- `cargo fmt` 通过。
- `cargo test -q active_runtime_config` 通过。
- `cargo test -q hook_stdin_timeout_defaults_and_rejects_invalid_values` 通过。
- `cargo test -q --test daemon_mode checkpoint_hook_stdin_times_out_when_host_keeps_pipe_open -- --nocapture` 通过。

### 2026-06-14：安装测试上传、无 note 兜底上传、北京时间和 hooksPath 根因修复

**变更原因：**

本轮现场问题集中暴露出“上传协议失败”和“安装生效失败”两类问题混在一起。服务端曾因 payload 缺少 `model` 报 `Field 'model' doesn't have a default value`，也曾因收到 RFC3339 纳秒 timestamp 而无法反序列化 `java.util.Date`。同时，`op-return-exchange` 的 `bfd32c8` 没有自动上传，最终根因不是远程接口，也不是 AI 归因计算，而是全局 `core.hooksPath` 指向已经不存在的旧 `yoavlax.ai-contribution-tracker\git-hooks` 目录，Git 提交阶段根本没有执行 post-commit；再叠加旧 `git-ai.exe bg run` 进程仍在跑，导致“安装了新版但真实提交仍可能不生效”。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/integration/install_test_upload.rs` | 安装成功后发送远程看板测试数据，且身份解析顺序改为 MCP / 环境用户 ID、Git 邮箱、IP 地址 | 安装成功后一定有可审计的测试上传尝试；没有 MCP `X-USER-ID` 时不再跳过，也不伪造测试用户 |
| `git-ai/src/integration/upload_stats.rs` | 所有 tool / prompt / breakdown 明细补齐 `model`，缺失时写 `unknown` | 避免后端 `git_ai_tool_stats.model` 无默认值导致整批上传回滚 |
| `git-ai/src/integration/upload_stats.rs` | commit timestamp 先解析 `%aI` 原始时区，再转北京时间 `yyyy-MM-dd HH:mm:ss` | 避免后端无法解析 RFC3339 纳秒时间，也避免 UTC 时间被直接截成错误的北京时间 |
| `git-ai/src/integration/upload_stats.rs` | 无 authorship note 的 commit 不再跳过，构造 `hasAuthorshipNote=false` 的 metadata-only payload | 历史 / 异常 commit 仍能上报项目、作者、时间、文件数和新增行，新增行进入 `unknownAdditions`，不制造 AI / 人工归因 |
| `git-ai/src/commands/git_ai_handlers.rs` | `upload-stats` 增加等待 authorship note / note 出现则跳过的内部参数和 debug 事件 | wrapper 兜底路径能先给 daemon 写 note 留时间，正常 note 出现时避免重复 metadata-only 上传 |
| `git-ai/src/commands/git_handlers.rs` | `git commit` 成功后后台启动兜底 `git-ai upload-stats <sha>` | 如果 post-commit / daemon 没及时写 note，仍能在等待超时后上传 metadata-only 记录，避免完全漏报 |
| `git-ai/src/commands/install_hooks.rs` | `install-hooks` 自动检测并清理失效的全局 `core.hooksPath` | 修复 `core.hooksPath` 指向不存在旧 hook 目录时 Git 完全不执行 post-commit 的根因 |
| `git-ai/src/commands/install_hooks.rs` | Windows 下重启 daemon 前按当前安装目录清理残留 `git-ai.exe bg run` | 即使 `daemon.pid.json` 缺失，也不会让旧后台进程继续接收 Trace2 / wrapper 事件 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填本轮根因、代码修改、现场恢复和验证要求 | 后续排查不再只看源码或版本号，必须同时核对安装版哈希、Trace2、hooksPath、daemon 与 debug 日志 |

**现场恢复步骤：**

1. 清理坏的全局 hooksPath：`git config --global --unset-all core.hooksPath`，或运行新版 `git-ai install-hooks` 自动清理。
2. 停掉旧后台进程：优先 `git-ai bg shutdown --hard`；如果 `daemon.pid.json` 缺失或仍有旧 `git-ai.exe bg run`，按进程路径清理同安装目录的后台进程。
3. rebuild release 后替换 `%USERPROFILE%\.git-ai\bin\git-ai.exe` 与 `%USERPROFILE%\.git-ai\bin\git.exe`，并用文件哈希确认两者与 `target/release/git-ai.exe` 一致。
4. 重新执行 `git-ai install-hooks`，确认 `core.hooksPath` 为空或指向存在目录，`trace2.eventTarget` 指向当前 git-ai pipe。
5. 确认只剩一个当前安装目录下的新 `git-ai.exe bg run`，再进行真实 commit 或 `upload-stats --dry-run` 验证。

**验证结论：**

- `cargo fmt --check` 通过。
- `cargo check --lib` 通过。
- `cargo test repair_stale_global_hooks_path --lib` 通过，覆盖缺失 hooksPath 会清理、存在 hooksPath 不误删。
- 现场安装版 `git-ai.exe` / `git.exe` 的 SHA256 已与 `target/release/git-ai.exe` 一致。
- 模拟坏 `core.hooksPath=...\yoavlax.ai-contribution-tracker\git-hooks` 后，安装版 `git-ai install-hooks` 输出 `Removed stale global core.hooksPath...`，最终 `core.hooksPath=<unset>`、Trace2 pipe 保留、只剩一个新 `git-ai.exe bg run`。
- `bfd32c8` 这类历史无 note commit 不补造 note；已按 `source=repair-missing-note` 上传 `hasAuthorshipNote=false` / `unknownAdditions=32` 的 metadata-only 记录，远程返回 200。

### 2026-06-15：席正浩 ibs-ecp-service 10:22 归因缺失与重复安装探活修复

**变更原因：**

`logs/席正浩.jsonl` 显示，用户机器在 `2026-06-15 10:07` 和 `10:23` 前后多次出现 `git-ai install success test (2.2.32)`，并且 `D:\test\ibs-ecp-service` 在 `2026-06-15 10:22:19` 左右的提交被反馈为“代码无法识别”。本轮排查先把两个现象拆开：

- `10:22` 这次不是“统计没有上传”。commit `4cf9b083b3cce5d57fd572a2f4fa194fc32c0752` 在 `10:22:30` 已经完成 HTTP 上传，服务端返回 `code=200`、`msg=git-ai记录创建成功`。
- 它也不是服务端字段映射错误。上传 payload 的本地统计就是 `gitDiffAddedLines=1`、`unknownAdditions=1`、`aiAdditions=0`。
- 真正断掉的是归因输入：`post_commit_working_log_loaded` 里只有 1 条 `human` checkpoint，`aiCheckpointCount=0`、`promptCount=0`、`sessionCount=0`、`attestationFileCount=0`。
- 同一用户同一版本在 `10:05` 的上一笔 commit `1563dba853c31d607da7081e2d42abaa12eb08b9` 仍有 `aiCheckpointCount=2`、`promptCount=2`、`aiAdditions=24`，说明归因引擎和上传链路本身能工作。
- 断档窗口发生在 `10:07` 自动安装 / `install-hooks` / daemon 重启 / 安装探活之后；`10:20-10:21` 期间日志只有 `git-ai blame --json --contents`，没有新的 `checkpoint github-copilot` 或 Copilot terminal checkpoint。

因此，这类现场不能用“只要用户说是 AI，就把 unknown 硬归成 AI”来修；那会破坏 git-ai 的审计语义。第一轮修复只是让自动更新和安装探活对用户提交链路更安静、更幂等，并强化日志完整性，避免同版本自动安装反复扰动 daemon / hook 状态，也避免坏 JSONL 让后续排查失真。它能降低再次断档和误判排查方向的概率，但并没有直接解决“为什么没有新的 `checkpoint github-copilot`”。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/integration/install_test_upload.rs` | 新增安装探活成功 marker：`~/.git-ai/internal/install_test_uploads.json`，key 由 producer、installer script、git-ai 版本、身份来源、身份值和上传 endpoint 共同计算 | 同一版本、同一用户身份、同一上传地址的 `git-ai install success test` 成功后只上传一次；后续同版本 `install-hooks` 只写 `install_test_upload_skipped(reason=already_uploaded_for_version_identity_and_endpoint)` |
| `git-ai/src/integration/install_test_upload.rs` | 新增 `install_test_uploads.lock` 跨进程锁，marker 读写和实际上传串行化；只有上传成功后才写 marker | 多个安装 / 自动更新进程并发触发时，不会在 marker 尚未落盘前重复打远程探活；失败不会被误记成成功，下一次仍可重试 |
| `git-ai/src/commands/upgrade.rs` | `AfterCommit` 更新检查不再无条件绕过缓存；只有 auto-update 未禁用、缓存 channel 匹配且缓存里已有 pending update 时，提交后才强制继续推进安装 | 提交后不会因为新鲜 no-update cache 被忽略而重复调度同版本安装链路；仍保留“已经确认有新版本”时尽快安装的能力 |
| `git-ai/src/diagnostics.rs` | `append_debug_event(...)` 写 `debug.jsonl` 前获取 `debug.lock`，锁内完成日志裁剪和单行追加 | 多个 git-ai 进程并发写日志时，不再把两条 JSON 行交错写坏；后续现场能可靠按时间线还原 checkpoint / install / upload 事件 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填本次现场证据、根因边界、代码修改、性能边界和验证结论 | 后续排查同类问题时，先区分“上传失败”“上传成功但 unknown”“重复安装探活”“AI checkpoint 缺失”，不再混成一个故障点 |

**性能边界：**

- 不在普通 git 命令路径做全系统进程扫描，也不按进程名高频杀进程。
- 安装探活 marker 锁只作用于 `install-hooks` / 安装成功探活路径；普通 `git commit`、`git blame`、`git-ai stats` 不会等待这个锁。
- marker 锁最长等待 30 秒，目的是在并发安装时让第二个安装进程看到第一个进程写下的成功 marker；超时后直接跳过探活，不阻塞用户提交链路。
- after-commit 自动更新继续复用后台调度，不把 commit 变成同步下载 / 同步安装；本次只收窄“什么时候需要检查 / 推进安装”的条件。
- debug 日志锁最长等待 2 秒；拿不到锁时放弃本条 debug 写入，优先保护用户命令响应和 JSONL 完整性。

**验证结论：**

- `cargo fmt` 通过。
- `cargo test --lib -q install_test_marker` 通过，覆盖 marker key 稳定性和重复写入只保留一条成功记录。
- `cargo test --lib -q install_test` 通过，覆盖安装探活 payload 标识、身份过滤、PowerShell 参数形用户值拒绝和 marker 行为。
- `cargo test --lib -q test_should_check_for_updates` 通过，覆盖 after-commit 尊重新鲜 no-update cache，以及 pending update 仍会触发安装推进。
- `cargo test --lib -q debug_log_lock_serializes_concurrent_writes` 通过，覆盖 debug JSONL 并发写入不会产生坏行。

### 2026-06-15：Copilot checkpoint 采集前置设置自愈

**变更原因：**

对席正浩这类 `unknownAdditions=1` 的现场继续下钻后，必须把问题说清楚：没有 AI checkpoint 时，post-commit 只能把新增行记为 unknown；这是正确的审计行为，不应该改成“凭用户口头确认就归 AI”。真正要修的是 checkpoint 为什么没有产生。

本轮确认有三类安装 / 自动更新后容易遗留的设置层断点：

- `~/.copilot/hooks/git-ai.json` 存在，不等于 VS Code 一定加载它；如果 `chat.useHooks=false`，Copilot native hook 不会执行。
- 如果用户 settings 里已经配置了 `chat.hookFilesLocations`，VS Code 会按这个 allow-list 加载 hook；一旦里面没有 `~/.copilot/hooks`，git-ai 写好的 Copilot hook 文件也不会被读取。
- Windows 安装器已经不再为新用户创建 legacy `git.exe` wrapper，残留的 VS Code `git.path` 如果仍指向 `C:\Users\admin\.git-ai\bin\git.exe`、不存在的 `.git-ai\bin\git`、或测试隔离泄漏出来的 `git-ai-test-home-*`，VS Code Git 链路会命中坏路径，出现 `Cannot Run Git: No such file ...`，并放大 checkpoint / commit 链路断档。

因此，修复不能是“安装时强行写 `git.path` 到 `.git-ai\bin\git.exe`”；那会在新用户机器上重新制造不存在的路径。正确做法是：只清理明确属于 git-ai 且已经坏掉的 `git.path`，让 VS Code 回到系统 Git；同时确保 Copilot hook 的 VS Code 加载开关和加载路径存在。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/mdm/utils.rs` | 新增 `update_vscode_copilot_hook_locations_settings(...)`，当 `chat.hookFilesLocations` 已存在时补齐 / 启用 `~/.copilot/hooks`；未配置该键时不主动新增 | 保持 VS Code 默认约定路径行为，同时修复“用户显式 allow-list 漏掉 Copilot hooks 目录”导致的无 checkpoint |
| `git-ai/src/mdm/utils.rs` | 新增 `repair_vscode_git_path_settings(...)`，只删除明显坏掉的 git-ai managed `git.path`：不存在的 `.git-ai\bin\git(.exe)`、`git-ai-test-home-*` 临时目录等；保留系统 Git 或用户自定义 Git | 避免自动安装后 VS Code 继续调用不存在的 git shim，也避免重新写入已被 Windows 安装器禁用的 legacy wrapper |
| `git-ai/src/mdm/agents/vscode.rs` | VS Code 安装器在配置 `chat.useHooks` 前先修复坏 `git.path`，随后补齐 Copilot hook allow-list | `git-ai install-hooks` / 安装脚本调用 VS Code 安装器时，可一次性修复 VS Code 侧导致 Copilot checkpoint 不触发的 settings |
| `git-ai/src/mdm/agents/github_copilot.rs` | GitHub Copilot 安装器自身也执行同样的 VS Code settings 自愈 | 即使 VS Code extension 安装器未命中或被跳过，只要检测到 Copilot / VS Code settings，也能修复 Copilot native hook 的必要前置条件 |
| `git-ai/src/mdm/utils.rs`、`git-ai/src/mdm/agents/github_copilot.rs` | 新增回归测试覆盖 `chat.useHooks=false`、`chat.hookFilesLocations` 缺少 `~/.copilot/hooks`、`git.path` 指向 `git-ai-test-home-*` 或不存在 shim | 锁定“hook 文件存在但没有 `checkpoint github-copilot`”的设置层断点，避免后续安装器回退 |

**性能边界：**

- 只读写 VS Code / Code Insiders 用户 settings 文件，不扫描全盘，不按进程名杀进程。
- 不主动修改项目 `.vscode/settings.json` 或 `.code-workspace`，避免改团队仓库配置；若工作区覆盖用户设置，仍按安装成功判断手册单独排查。
- 不无脑写 `git.path`。当前 Windows 新安装不会创建 `git.exe` wrapper，写入不存在路径比不写更坏；本次只删除明确坏掉的 git-ai managed 路径。
- `chat.hookFilesLocations` 未配置时不新增该键，因为 VS Code 默认会扫描 `~/.copilot/hooks`；只有用户显式配置 allow-list 时才补齐 `~/.copilot/hooks`。

**验证结论：**

- `cargo fmt` 通过。
- `cargo check -q` 通过。
- `cargo test --lib -q update_vscode_copilot_hook_locations` 通过，覆盖补齐 / 启用 `~/.copilot/hooks` 以及未配置 allow-list 时不写入。
- `cargo test --lib -q repair_vscode_git_path_settings` 通过，覆盖删除不存在 git-ai shim、删除 `git-ai-test-home-*` 泄漏、保留系统 Git。
- `cargo test --lib -q test_install_extras_repairs_vscode_settings_for_copilot_hooks` 通过，覆盖 Copilot 安装器独立修复 VS Code settings 的端到端行为。
- `cargo test --lib -q github_copilot` 通过，Copilot 模块 57 个测试全部通过。

### 2026-06-17：现场日志治理、Copilot `dirtyFiles` 路径兜底、recent `KnownHuman` 保护与自动更新上传顺序修复

**变更原因：**

6 月 16 日到 6 月 17 日的现场日志暴露出四类问题混在一起，必须拆开处理：

| 现场样本 | 表现 | 结论 |
|------|------|------|
| `张艺锋.jsonl` | 日志文件异常膨胀，6 月 15 / 6 月 16 以外的历史噪音影响判断 | 先做日志治理，只保留目标日期窗口；历史异常内容移入备份，不再参与本轮归因判断 |
| `庞福泽.jsonl` / `rj-ltc-contract-web`、`徐建丰.jsonl` / `ai-cr-manage-service`、`胡晴琨.jsonl` / `op-api` | 代码实际由 Copilot / AI 生成，但大量文件落入 `unknown` 或部分被误算为人工 | Copilot hook 有时没有在 `tool_input` / `tool_response` 中提供路径，真实路径只存在于顶层 `dirtyFiles` / `dirty_files` 的 key；旧代码没有用这些 key 兜底 |
| `胡晴琨.jsonl` / `op-api` / `feature-20260601-ebg-ecp-h` / `2026-06-17 16:29:36` | 提交 `23a81b7cc8a45fbc849630b957681f884b856c5f` 实际应全部为 AI，统计为 `aiAdditions=20`、`humanAdditions=43`、`unknownAdditions=0` | 用户使用的是 `2.2.39`，不是旧版本问题；`AI Edited` 后紧跟 IDE `known_human` 保存事件，旧归因会让保存事件重新占有已有 AI 行 |
| `张彪.jsonl` / `contract` / `2.2.37` | `2026-06-16 15:17:43`、`15:02:23`、`14:59:24` 代码实际人工写入，但归因为 `unknown` | 这是 legacy `Human` checkpoint 明确人工新增未生成 `h_*` attestation 的一类问题，应由“legacy Human checkpoint 明确人工新增不再落入 unknown”修复覆盖；旧用户必须升级到实际包含该修复的 release asset |
| commit 后自动更新 | 自动更新在 commit 后启动，可能重启 / 替换 runtime，导致本次 commit 的远程看板记录缺失 | commit 主流程必须先给本次 commit 的上传留出机会，再调度后台自更新；自动更新功能仍保留，但不能抢在本次 commit 上传之前扰动 runtime |

**为什么 hook 的 `tool_input` / `tool_response` 没抽到文件路径：**

Copilot native hook 和 Copilot CLI hook 的 payload 并不总是把编辑文件放在 `tool_input.file_path`、`tool_input.path`、`files[]` 或 `tool_response` 里。真实现场里，`tool_input` / `tool_response` 可能只包含旧文本、新文本、执行结果、脱敏后的 `...`，或者只表达“这次工具调用做了编辑”，不带可直接解析的路径。与此同时，hook 顶层会带 `dirtyFiles` 或 `dirty_files`，其中 key 就是当前 dirty 文件的绝对 / 相对路径，value 是文件快照内容。旧实现只把这份 map 传给 daemon 当内容快照使用，没有在路径抽取失败时把 map 的 key 当作编辑路径，因此会出现“内容在 payload 里，但 file_paths 为空，checkpoint 被跳过”的漏归因。

本次修复把 `dirtyFiles` / `dirty_files` 的 key 作为严格兜底：只有 `tool_input` / `tool_response` 路径抽取为空时才启用；仍然只使用当前 hook payload 中携带的 dirty 文件，不扫描会话历史，也不把非当前工具调用的路径合并进来。

**为什么 `AI Edited` 后会紧跟 IDE `known_human`：**

`AI Edited` 是 Copilot hook 对“AI 工具完成编辑”的记录；`known_human` 来自 VS Code / IDE 的保存、文档变更或文件系统监听。AI 把内容写进编辑器或磁盘后，IDE 会像普通保存一样触发这类监听，所以两条事件紧挨着出现是正常竞态，不代表用户在几百毫秒内手工重写了整块代码。旧逻辑只看到了“IDE known_human 保存发生在后面”，就可能把已经由 AI 写入的行重新归到人工。

当前修复保留两个边界：

- 1 秒内紧随 AI checkpoint 的 `KnownHuman` 仍按原策略拒绝，避免保存事件直接抢占 AI 编辑。
- 30 秒内出现 recent AI file state 时，`KnownHuman` 不再重领已有 AI 行，只对相对上一状态真正新增 / 改动的行生成 `h_*` 人工归因。这样用户确实在 AI 后手动补了一两行时仍能计入人工，但 AI 已有大块不会被 IDE 保存事件改成人工。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/commands/checkpoint_agent/presets/github_copilot/mod.rs` | 新增 `file_paths_from_dirty_files(...)`，把 `dirtyFiles` / `dirty_files` map 的 key 规范化、排序、去重后作为路径兜底 | 当 Copilot payload 的 `tool_input` / `tool_response` 没有路径时，仍能把当前 dirty 文件送入 AI checkpoint |
| `git-ai/src/commands/checkpoint_agent/presets/github_copilot/ide.rs` | VS Code native hook 在 `extract_filepaths_from_vscode_hook_payload(...)` 为空时回退到 `file_paths_from_dirty_files(...)` | 覆盖 `op-api`、`rj-ltc-contract-web`、`ai-cr-manage-service` 中 Copilot native 编辑后大量 unknown 的场景 |
| `git-ai/src/commands/checkpoint_agent/presets/github_copilot/cli.rs` | Copilot CLI hook 同步使用 `dirty_files` 路径兜底 | terminal / CLI 型 Copilot 编辑在工具结果无路径时不再直接丢 checkpoint |
| `git-ai/src/daemon/checkpoint.rs` | `PreviousFileState` 增加 `timestamp`；新增 `KNOWN_HUMAN_RECENT_AI_SAVE_LIMIT_SECS=30` 和 `has_recent_ai_file_state(...)`；recent AI 后的 `KnownHuman` 只限制到真实 changed lines | 修复 `AI Edited` 后 IDE 保存事件把已有 AI 行抢成人工的问题，同时保留用户后续手动补充行的人工归因 |
| `git-ai/src/commands/git_handlers.rs` | `run_post_commit_followups(...)` 先启动本次 commit 的 fallback `upload-stats`，再调用 `maybe_schedule_background_update_check_after_commit()` | 自动更新功能继续正常执行，但不会先重启 / 替换 runtime，导致本次 commit 的远程看板记录丢失 |
| `git-ai/tests/integration/pending_ai_edit_suppression.rs` | 新增 `test_recent_known_human_save_after_ai_does_not_reclaim_ai_lines` | 锁定“AI 两行 + IDE 保存补一行”时，已有 AI 行仍为 AI，新增手工行计入 human |

**版本与历史数据口径：**

- `胡晴琨.jsonl` 已只保留 `2026-06-17` 日志，过滤前备份为 `logs/胡晴琨.jsonl.bak-before-20260617-filter`；`徐建丰.jsonl`、`张彪.jsonl` 同样按 `2026-06-16` 做日期窗口清理。清理只影响本地排查样本，不会修改已经上传到远程看板的历史记录。
- 张彪机器仍使用 `2.2.37` 时，不能用当前源码结论直接推断用户机已修复；必须以用户机 `git-ai --version`、`debug.jsonl` 中的 version、实际下载的 release asset 自报版本共同确认。若 `2.2.38` 的发布 asset 未包含 legacy `Human` checkpoint 修复，仍需要升级到包含 2.35/2.42/2.43/2.44 这些修复点的后续版本。
- 对已经提交且本地 authorship note 缺少 AI checkpoint 的历史 commit，客户端不会凭用户口头说明把 `unknown` 硬改成 AI；只能通过重新触发正确 checkpoint 后再提交，或按人工确认流程做显式历史修复 / 回补，避免破坏审计语义。
- 看板侧对历史脏数据应区分三类：没有 authorship note 的 metadata-only 记录保持 `unknownAdditions`；旧 IDE 保存误归因记录需要结合版本和日志确认；安装探活 / 旧外部 producer 脏数据继续按前述 `installTestProducer` / `installerScript` 隔离。

**验证结论：**

- `cargo test test_copilot_native_uses_dirty_files_when_tool_paths_missing`
- `cargo test cli_post_uses_dirty_files_when_tool_paths_missing`
- `cargo test test_recent_known_human_save_after_ai_does_not_reclaim_ai_lines --test integration`
- `cargo fmt --check`
- `git diff --check`

### 2026-05-10：补充 Windows 真机验证结论与 GitHub latest / asset 修正流程

**变更原因：**

本轮真实排查已经证明，现场问题至少分成两段独立链路。第一段是运行时链路：即使源码已经修复、安装目录里的 `git-ai.exe` / `git.exe` 也已经替换，如果常驻 daemon 没有重启，post-commit 仍然会继续跑旧逻辑，最终表现成 prompt note 还是空的。第二段是发布链路：即使本机已经装上 `2.1.11`，如果 GitHub `releases/latest/download/...` 仍然跳到旧 tag，或者新 tag 下的 asset 本体版本不对，外部默认安装 / 更新链路还是拿不到 `2.1.11`。旧文档只写了“默认跟随 latest”，没有把这两个闭环明确拆开，也没有写清楚 latest 修正的实际操作步骤。

**本次文档级补充：**

| 项目 | 补充内容 | 影响 |
|------|----------|------|
| Windows 真机验证结论 | 补充“提示词恢复”必须在同步安装版 `git-ai.exe` / `git.exe` 后再执行 `git-ai bg restart`，否则 post-commit 仍可能由旧 daemon 处理 | 避免后续再把“源码已修好但真实 commit 还是空 note”误判成解析代码没生效 |
| latest 语义澄清 | 明确 `GIT_AI_RELEASE_TAG=latest` 只在 GitHub latest 指向正确稳定版、且 asset 本体版本正确时才成立 | 避免把本机升级失败全部归因到 installer / auto-update 代码 |
| 发布侧修正流程 | 新增一整段可执行的 latest / asset 修正步骤，明确使用 `.github/workflows/release.yml` 的 `release_production=true` 生产稳定 release，并要求二次验证 302 跳转和 asset `--version` | 让“怎么把外部自动更新真正修到 2.1.11”有了标准操作步骤，而不再停留在口头结论 |

**验证结论：**

- 本机真实验证中，重启新 daemon 后，最终 commit `7febcecf61f151f5e73eb61164088fc27c56cb72` 的 ai note 已经不再是空 `messages`，并能识别出 `9` 条 user message。
- 同一轮排查里，`/releases/latest/download/git-ai-windows-x64.exe` 仍然曾跳到 `v2.1.9`，说明 GitHub latest 指向本身不会因为本地代码修复而自动纠正，必须单独按 release 流程修正。
- 之后所有关于“默认 latest 应该拿到哪个版本”的判断，都应以 GitHub latest 的 302 跳转结果和实际下载 asset 的 `--version` 为准，而不是只看仓库源码或本机 debug build。 

### 2026-05-31：自动上传 activity lock 超时兜底

**变更原因：**

黄芳机器的最新 `debug(11).jsonl` 与前一轮日志不同：本次已经进入 `post_commit` 和自动上传路径，且 payload、用户 ID、默认上传 URL 都已准备好；失败点是 `upload_stats_activity_lock_timeout`，等待 `C:\Users\admin\.git-ai\internal\upload_activity.lock` 约 5 秒后直接失败，日志中没有任何 HTTP request / response 事件。因此这不是后端接口失败，也不是安装 wrapper 没触发 post-commit，而是本地上传 activity lock 在 HTTP 之前阻断了自动上传。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/integration/upload_stats.rs` | 新增 `upload_activity_lock_bypass_allowed(...)`，只允许 `background` / `inline` 自动上传在 lock 超时后继续；手动 `manual` 上传仍不绕过 | 自动上传不再因为陈旧锁或旧 runtime 残留句柄永久停在 HTTP 前；主动补传仍保持严格串行语义 |
| `git-ai/src/integration/upload_stats.rs` | 自动上传 lock 超时时新增 `upload_stats_activity_lock_bypassed` 诊断事件，并继续执行 status / payload / HTTP 上传流程 | 现场日志能直接区分“锁超时后已继续发 HTTP”和“仍在本地锁前失败”两类问题 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填本次 activity lock 根因、诊断事件和 Phase 清单 | 看板方案继续与当前上传实现保持一致 |

**验证结论：**

- 本地已通过 `cargo test --features test-support --lib upload_activity_lock`，覆盖 activity lock 路径、等待时长和自动 / 手动绕过边界。
- `task test TEST_FILTER=upload_activity_lock CARGO_TEST_ARGS="--lib"` 因本机未安装 `task` runner 无法执行，已用等价 cargo 命令完成窄验证。
- 若后续现场日志出现 `upload_stats_activity_lock_bypassed` 后仍无 `upload_stats_http_request_ready`，再继续查本地状态文件 / payload 构造；若已有 HTTP 事件，则根因已经进入网络或服务端响应层。

### 2026-05-08：commit 成功后复用后台升级链路触发 CLI 自更新

**变更原因：**

前面的设计已经把需求 1 明确成“安装或更新 Spec Kit 时，都要把 git-ai 升级到目标最新版本”，但真实日常使用里，用户更常遇到的问题是本机 `git-ai` 版本已经落后，却只有在 `push` 后才有机会触发后台升级检查。这样排查“为什么本机还是旧版本”时，触发时机太靠后，也和“commit 时自动更新”的新要求不一致。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/commands/git_handlers.rs` | 抽出“successful commit 后续动作”的统一判定，并在 daemon 直连路径与 daemon 不可用的直通 git 路径里都补上 `upgrade::maybe_schedule_background_update_check()` | `git commit` 成功后即可复用现有后台升级检查/安装机制，提交失败或非 commit 命令不会误触发 |
| `git-ai/README.md` | 补充 `git commit` 成功后会调度后台自更新检查的说明 | 用户侧文档与当前 CLI 行为保持一致，减少“为什么 push 会更、commit 不会更”的认知偏差 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填本次 commit 自更新实现补充、Phase 1 清单和验证结论 | 看板方案与当前代码状态一致，不再只停留在最初安装链路设计 |

**验证结论：**

- 已新增并跑通窄单测 `cargo test --lib post_commit_followups_require_successful_commit`，确认只有成功的 `git commit` 会进入这条后续动作分支。
- 当前 commit 自更新复用的是既有 `upgrade --background` 机制，因此仍然遵守现有节流、版本检查间隔、channel 选择和 auto-update 开关，不会把 commit 变成同步下载/安装操作。
- 当前实现的“最新版本”仍指配置 channel 下的已发布 release，而不是直接跟踪 GitHub 仓库某个分支 HEAD 或在本机构建最新源码。

#### 这轮 commit 后自动更新链路踩过的坑

| 坑位 | 现场表现 | 复盘结论 |
|------|----------|----------|
| commit 成功后已触发后台检查，但真实提交链路还在跑旧版本 | 本机以为“代码已经修好”，真实 commit 仍继续产出旧行为 | 触发点和执行点不是一回事；native hook、wrapper 和 post-commit 最终执行哪个 binary，取决于 hook JSON、安装目录路径和常驻 daemon runtime，而不是只看源码是否已 merge |
| 安装目录 `git-ai.exe` / `git.exe` 已被新版本覆盖，但结果仍不对 | `git-ai --version` 看起来更新了，post-commit / upload 仍走旧逻辑 | 替换磁盘文件不会替换正在内存里跑的 daemon；commit 后自更新如果不伴随 runtime handoff 或显式 `git-ai bg restart`，很容易出现“文件新、daemon 旧” |
| `bg restart --hard` 后仍像是打到了旧 daemon / 旧 pipe | 安装后看似已经重启，trace2 / post-commit 仍不稳定 | Windows 下 daemon runtime、control socket 和全局 `trace2.eventTarget` 是绑定的；只重启进程不刷新 trace2 配置，wrapper 仍可能继续把事件写到旧 pipe |
| latest / release asset 和源码版本不一致 | `latest/download` 仍跳旧 tag，或者下载到的 asset 自报版本不对 | “本机源码修好”与“外部默认自动更新能拿到正确版本”必须拆成两个闭环；发布后必须同时核对 GitHub latest 的 302 跳转和下载后二进制的 `--version` |
| 安装器在 `Checksum verified` 之后长时间无输出 | 用户误以为下载卡住或脚本死锁 | 这个阶段很可能不是下载，而是卡在 `Acquire-UploadActivityLock` 或后续 `Wait-ForFileAvailable`；如果安装器不打印等待对象，现场很难第一时间分辨是 upload lock 还是 exe 文件锁 |
| 安装器反复打印 `Stopping lingering git-ai processes: ...` 但 kill 不掉 | `taskkill` 提示“没有此任务的实例在运行”，安装脚本仍不断重复同一批 PID | 仅靠 `Win32_Process` 枚举会把 ghost PID / 已退出进程对象也算进 blocker；再叠加 `Stop-Process` 错误被吞掉，脚本输出会持续误导现场 |
| 安装器一直等 `git-ai.exe` 可写 | 运行中的旧 `git-ai.exe` / hook 子进程仍持有 exe 镜像，安装器无法用独占写句柄打开目标文件 | 不能把“无法独占写打开”当作必须等待的失败条件。Windows 支持先把运行中的 exe 重命名到同目录 retired 路径，再把新 exe 放回稳定路径；安装器应使用 retired binary 替换策略，并对同安装目录进程使用 `taskkill /F /T /PID` 清完整进程树 |
| `checkpoint ... --hook-input stdin` 残留进程锁住安装目录 | VS Code / Copilot hook 链路出现卡死，`git-ai.exe` 无法替换 | hook 调用方必须写完并关闭 stdin；否则会留下挂死的 checkpoint 进程。即使 replacement runtime 能绕过旧锁继续服务，如果 runtime 拷贝和过期锁不回收，还会进一步放大 `~/.git-ai/internal` 膨胀问题 |

### 2026-05-04：修复上传成功误判，并补齐服务端 `git_ai_tool_stats` 主键类型

**变更原因：**

对 `/api/public/upload/ai-stats` 的联调和只读数据库核查表明，现场同时存在两条独立问题链：一是 `git-ai` 客户端当前只按 HTTP 状态码判断上传成功，导致服务端即使返回 HTTP 200 + `{"code":500,"msg":"..."}`，本地仍会误记为 `upload_stats_succeeded`；二是 `ai-cr-manage-service` 的 `git_ai_tool_stats.id` 仍然使用 `INT/Integer`，但全局主键策略已是 `ASSIGN_ID` 雪花 long，现网表里已经出现负数主键和边界值，最终会把工具统计插入打成 `Duplicate entry ... git_ai_tool_stats.PRIMARY`。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `ai-cr-manage-service/ruijie-codereview-manage/src/main/java/com/ruoyi/codereview/gitai/domain/po/GitAiToolStats.java` | 将 `id` 从 `Integer` 改为 `Long` | 服务端实体与 MyBatis-Plus `ASSIGN_ID` 的 snowflake long 对齐，不再在 Java 侧先把主键截断成 32 位 |
| `ai-cr-manage-service/ruijie-codereview-manage/src/main/java/com/ruoyi/codereview/gitai/service/impl/GitAiStatsServiceImpl.java` | `appendToolStats(...)` 显式补 `IdWorker.getId()` | 工具统计记录与 summary / commit / file / prompt 记录保持同样的主键生成方式，避免依赖隐式行为 |
| `git-ai/src/integration/upload_stats.rs` | 对 HTTP 2xx 响应继续解析 JSON body；若 body 中存在 `code` 且不等于 `200`，则把上传视为失败，并把 `code/msg` 写入 debug 事件 | 现场再出现“HTTP 200 但业务失败”的响应时，自动上传和手动 `upload-stats` 不会继续误报成功 |
| `git-ai/docs/design-doc/git_ai_tool_stats_primary_key_fix_2026-05-04.sql` | 新增只读核查 SQL 和手工 DDL | 数据库保持人工执行，避免直接动现场库；同时把需要核查和执行的 SQL 固化下来 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填本次上传成功判定和服务端主键修复的实现补充、Phase 清单和代码修改摘要 | 看板文档继续与当前实现状态一致 |

**验证结论：**

- 只读数据库核查已确认 `cr.git_ai_tool_stats.id` 当前仍是 `INT`，`cr.git_ai_commit_stats.id` 是 `BIGINT`，且 `git_ai_tool_stats` 中已存在负数主键和接近 32 位边界的值。
- `git-ai` 本地 `debug.jsonl` 已能证明 `57b254e2` 这次上传拿到了 HTTP 200，但同一套后端环境中的 `git_ai_commit_stats` 里查不到该 commit，说明“只看 HTTP 2xx”确实会把业务失败误记成成功。
- 本轮没有直接执行数据库 DDL；数据库列改造仍需由人工在合适窗口执行 `docs/design-doc/git_ai_tool_stats_primary_key_fix_2026-05-04.sql` 中的 `ALTER TABLE`。

### 2026-05-04：修复 AI follow-up checkpoint 只保留最后几行 AI 归因

**变更原因：**

对 `7dac9daa2aeb199bbae2fe5985531bd0011715bd` 这类现场继续下钻后确认：问题不在 `git-ai stats` 或 `src/integration/upload_stats.rs` 的统计公式，而在 checkpoint 链路复用旧 previous state 时拿到了一份“已经被持久化层裁掉字符归因”的旧基线。`RepoStorage::append_checkpoint()` 为节省 working log 体积，会通过 `prune_old_char_attributions()` 清空同文件旧 checkpoint 的 `entry.attributions`，仅保留 `line_attributions`；但 `checkpoint.rs` 旧实现的 `build_previous_file_state_maps()` 在 AI checkpoint 需要回退到更早 previous checkpoint 时，仍直接使用这个空的 `entry.attributions`。后续 `make_entry_for_file()` 看见旧字符归因为空，就会把未直接覆盖的旧 AI 大块内容重新补成 human，最终 follow-up AI checkpoint 只剩本次小改那几行还是 AI。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/commands/checkpoint.rs` | `build_previous_file_state_maps(...)` 新增 `working_log` 参与；当旧 checkpoint 的 `entry.attributions` 已被裁空但 `line_attributions` 仍在时，使用对应 blob 内容把 `line_attributions` 还原为字符归因，再作为 `PreviousFileState.attributions` 提供给后续 checkpoint diff | AI follow-up checkpoint 再回退到较早 AI 基线时，不会再把前一轮大块 AI 代码误补成 human，最终 note / stats 不再缩成“只剩最后几行 AI” |
| `git-ai/src/commands/checkpoint.rs` | 新增 `test_ai_follow_up_checkpoint_preserves_previous_ai_ranges` 端到端回归测试，覆盖 `KnownHuman 大改 -> AI 大改 -> KnownHuman 小改 -> AI 小改` 的真实链路 | 以后如果旧 checkpoint 字符归因再次被错误丢失，这条误归因会在单测阶段直接暴露 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填本次根因、Phase 2 清单与验证结果 | 看板方案继续与当前修复状态保持一致 |

**验证结论：**

- `cargo test test_ai_follow_up_checkpoint_preserves_previous_ai_ranges --lib` 已从失败转为通过；修复前它稳定复现“最终只剩 `LineAttribution { start_line: 25, end_line: 26, ... }`”这类缩减现象，修复后已恢复为保留整块历史 AI 归因。
- 邻近回归 `cargo test test_ai_checkpoint_reclaims_recent_known_human_file_capture --lib` 也已通过，说明本次“还原被裁剪旧基线”没有破坏上一轮 recent `KnownHuman` reclaim 修复。

### 2026-05-04：prompt 持久化与上传仅保留用户输入

**变更原因：**

现场继续下钻后确认，`promptText` 并不是把 assistant 输出拼进去了；它原本就只会从 `Message::User` 提取文本。真正导致“提示词里还保存了 AI 自己输出”的，是 `PromptRecord.messages` 在 `post_commit` 前会原样保留 checkpoint transcript 中的 assistant / thinking / plan / tool_use 消息，而上传 payload 里的 `messages` 字段也会直接把这整段 transcript 序列化出去。结果就是服务端虽然 `prompt_text` 只展示用户输入，但 note、CAS、本地 SQLite 以及上传 payload 的 `messages[]` 仍然能看到 AI 输出。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/authorship/secrets.rs` | 新增 `retain_user_prompt_messages(...)`，统一把每个 `PromptRecord.messages` 过滤成仅保留 `Message::User` | 后续进入 note、CAS 或本地 SQLite 的 prompt 记录不再携带 assistant / tool transcript |
| `git-ai/src/authorship/post_commit.rs` | 在进入 `PromptStorageMode::Local` / `Notes` / `Default` 分支前统一执行 `retain_user_prompt_messages(...)` | 无论最终 prompt 存到 notes、CAS 还是本地 SQLite，持久化内容都只剩用户输入 |
| `git-ai/src/integration/upload_stats.rs` | `serialize_prompt_messages(...)` 改为只序列化 `Message::User`；对应测试从“3 条消息”改为“2 条用户消息” | 对历史 note 中已存在的 assistant/tool transcript 也会在上传时自动裁掉，远程 payload 不再带 AI 输出 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填当前语义：`promptText` 只取用户输入，`notes` 模式下持久化的也是用户输入 prompt，而不是完整 transcript | 文档和当前实现保持一致，避免继续按“完整 prompt 全量上传”理解 |

**验证结论：**

- `cargo test retain_user_prompt_messages_drops_non_user_entries --lib` 通过，说明写入前的 `PromptRecord.messages` 已能稳定裁掉 assistant 和 tool_use 消息。
- `cargo test build_prompt_stats_includes_prompt_text_and_messages --lib` 通过，说明上传 payload 里的 `promptText` 仍然是 `first prompt\n\nsecond prompt`，而 `messages[]` 已从 3 条收敛为 2 条 `user` 消息。

### 2026-05-04：修复 UTF-8 文件路径导致的逐文件统计失真

**变更原因：**

对最近两次提交 `bef6f7df` / `d9d879dc` 的核对表明：commit 级 `aiAdditions=237` 是本地 note + `git-ai stats` 共同给出的真实结果；真正错误的是逐文件 payload 在中文文件名场景下的路径键不一致。`git diff-tree --numstat` 默认受 `core.quotepath=true` 影响，会把非 ASCII 路径写成带引号和字节转义的字符串，而 authorship note 里的 `attestation.file_path` 保持 UTF-8 原文，最终导致文件级 `aiAdditions` 查不到对应键。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/integration/upload_stats.rs` | `git_diff_tree_numstat(...)` 改为显式调用 `git -c core.quotepath=false diff-tree --numstat ...` | 非 ASCII 文件路径在 `stats.files[]` 中恢复为正常 UTF-8，逐文件 AI 归因不再因为路径转义而整体掉到 `unknownAdditions` |
| `git-ai/src/integration/upload_stats.rs` | 新增 `git_diff_tree_numstat_returns_unquoted_utf8_paths` 回归测试 | 后续即使再改 upload payload，也能及时发现中文文件名再次被 Git 转义的问题 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填最近两次 commit 的核对结论、实现约束和 Phase 2 验收项 | 看板文档与实际修复状态保持一致 |

**核对结论：**

- `d9d879dc`：整次提交只有一个中文文件 `docs/design-doc/git-ai环境变量与配置说明.md`，本地 note 明确记录 `1-237` 都是 AI；旧 payload 因路径转义不匹配，把这个文件错误上报成 `aiAdditions=0`。
- `bef6f7df`：commit 级 `aiAdditions=237` 同样是本地真实结果；`src/diagnostics.rs=41`、`src/integration/upload_stats.rs=71` 等文件级 AI 行数来自 authorship note 中实际落盘的 AI 行范围，并不是服务端把整文件本该全算 AI 却映射错了。

### 2026-05-04：修复 whitespace-only 新增行被误记为 `unknownAdditions`

**变更原因：**

在 `op-return-exchange` 的 `FastFifthTest.java` 现场里，`git diff HEAD^ HEAD` 明确显示新方法一共新增了 7 行，其中包含首尾两个空白分隔行；但对应 commit 的 authorship note 只记录了 `31-36` 这 6 行，导致末尾那个空白行没有直接落进 note 的 `line_ranges`。旧实现无论是 `git-ai stats` 还是上传 payload 的 `stats.files[]`，都只是拿“本次 commit 的新增行号”与 note 的 `line_ranges` 直接求交，因此最后会出现 `gitDiffAddedLines=7`、`humanAdditions=6`、`unknownAdditions=1`、`aiAdditions=0` 这种看起来“总是差 1 行”的结果。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/authorship/stats.rs` | 新增按文件的 accepted-line 统计结构，并在计算 commit 级 `humanAdditions` / `aiAccepted` 时，对未直接 attestation 的 whitespace-only 新增行做邻接归并：若该空白新增行紧邻一侧或两侧一致的 KnownHuman / AI 已归因新增块，就跟随相邻块一起计数 | `git-ai stats <sha> --json` 在“方法前后带空白分隔行”的场景下，不再把尾部空白行单独记成 `unknownAdditions` |
| `git-ai/src/integration/upload_stats.rs` | 文件级 `stats.files[]` 改为复用 `src/authorship/stats.rs` 的同一套按行归并结果，而不是继续单独按 `line_ranges` 粗略求和 | 上传 payload 的文件级 `humanAdditions` / `aiAdditions` 与本地 `git-ai stats` 口径保持一致，不会再出现“一边修好了、另一边还差 1 行”的分叉 |
| `git-ai/src/authorship/stats.rs`、`git-ai/src/integration/upload_stats.rs` | 各补 1 条回归测试，覆盖“KnownHuman 代码块后面跟一个新增空白行”的场景 | 后续如果再有人改 stats / upload 计数链路，这类空白分隔行回退到 `unknownAdditions` 会更早暴露 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 回填本次空白新增行误统计的根因、Phase 2 验收项和代码修改摘要 | 看板文档继续与当前实现状态一致 |

**验证结论：**

- 静态校验已通过：`src/authorship/stats.rs`、`src/integration/upload_stats.rs` 当前无编辑器错误，`git diff --check -- src/authorship/stats.rs src/integration/upload_stats.rs` 无输出。
- 原先阻塞执行验证的 Windows GNU 链接器问题已在后续“Windows LLVM 工具链固化”修复中解除：当前工作区默认改走 `x86_64-pc-windows-gnullvm`，`cargo test debug_timestamp_uses_beijing_time --lib` 已可执行并通过，不再存在“缺少 gnullvm / 仍被 `_Unwind_Resume` / `_GCC_specific_handler` 挡住”这条环境阻塞。

### 2026-05-04：固化 Windows LLVM 工作区构建链路

**变更原因：**

前面多轮定位已经证明：当前 Windows 机器上真正拦住 Rust 可执行验证的，不是 `debug_timestamp_uses_beijing_time` 本身，也不是 `diagnostics.rs` 的实现，而是 PATH 优先命中了 `Rust stable GNU 1.95` 和 `D:\MinGW\mingw64\bin` 里的 GCC 8.1 `sjlj` 运行时。这组 `windows-gnu` + 旧 MinGW runtime 组合会在链接阶段报 `_Unwind_Resume` / `_GCC_specific_handler`。一旦临时把 PATH 前面切到 `Rust stable LLVM 1.95`（`host=x86_64-pc-windows-gnullvm`）和 LLVM-MinGW，同一条窄测试就不再报这组 GNU unwind 符号错误，说明根因就是工具链而不是源码。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/.vscode/settings.json` | 为 `terminal.integrated.env.windows`、`rust-analyzer.cargo.extraEnv`、`rust-analyzer.check.extraEnv` 统一前置 `Rust stable LLVM 1.95` 和 LLVM-MinGW 的 PATH | 新开的 VS Code 终端以及 rust-analyzer 触发的 cargo/check 默认不再走 `Rust stable GNU 1.95` + `D:\MinGW`，从源头规避 `_Unwind_Resume` / `_GCC_specific_handler` |
| `git-ai/src/authorship/stats.rs` | 修复 4 个仍在调用旧 `accepted_lines_from_attestations(...)` 三参签名的测试，补齐新的 `repo + commit_sha` 依赖 | 环境切到 LLVM 后，lib tests 不会再因为旧测试签名编译错误而把验证挡住 |
| `git-ai/docs/design-doc/git-ai环境变量与配置说明.md`、`git-ai/docs/design-doc/git-ai看板方案.md` | 回填 Windows 工具链排查方式、工作区设置和最新验证结论 | 后续再遇到 Windows 链接问题时，文档能直接指导定位到 `cargo -vV` 的 host triple 与 PATH 来源 |

**验证结论：**

- 在与工作区设置一致的 LLVM PATH 下，`cargo test debug_timestamp_uses_beijing_time --lib` 已经成功编译并执行，结果为 `test diagnostics::tests::debug_timestamp_uses_beijing_time ... ok`。
- 这次执行中已经不再出现 `_Unwind_Resume` / `_GCC_specific_handler`，说明当前阻塞确实已从“链接器坏了”切换回正常的 Rust 编译/测试路径。
- 当前剩余的只有一个非阻塞 warning：`src/authorship/stats.rs` 中的 `line_range_overlap_len` 尚未被使用；它不影响本轮工具链修复与测试通过。

### 2026-04-30：新增原生主动上传命令

**变更原因：**

自动上传解决的是“commit 之后尽快上报”，但现场仍需要一个显式、可控、不会依赖 post-commit 时机的主动上传入口，用于补传当前 commit、指定 commit，或在定位上传问题时先做 dry-run 预览。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/integration/upload_stats.rs` | 新增 `upload_local_commit_stats(...)`，复用现有 note/stats/payload/HTTP 上传逻辑，并允许 `source` 区分 `auto` / `manual` | 手动命令与自动上传复用同一套 payload 和接口，不会分叉出第二条上传实现 |
| `git-ai/src/commands/git_ai_handlers.rs` | 新增 `upload-stats` / `upload-ai-stats` 子命令，支持 `--dry-run`、`--source`、多个 commit 和默认 `HEAD` | 开发者可以显式上传本地已有统计结果，而不必依赖 post-commit 自动触发 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 补充原生命令用法和阶段验收项 | 文档与当前代码保持一致 |

**行为边界：**

这个命令是新增入口，不会修改 `post_commit` 自动上传是否开启的判定，也不会替代 `upload-ai-stats.ps1` 的批量 / 范围回补能力。默认调用只影响显式指定的 commit 集合；不带参数时仅处理 `HEAD`。

**命令语法：**

```bash
git-ai upload-stats [<commit>...] [--dry-run] [--source <name>] [--ignore <pattern> ...]
git-ai upload-ai-stats [<commit>...] [--dry-run] [--source <name>] [--ignore <pattern> ...]
```

**使用前提：**

- 本地目标 commit 有 authorship note 时，会上传真实 note / stats / prompt 归因。
- 如果目标 commit 没有 authorship note，命令不会伪造 AI 或人工归因，也不会跳过；它会基于 Git diff 构造 metadata-only payload：`hasAuthorshipNote=false`、`prompts=[]`、`aiAdditions=0`、`humanAdditions=0`、`unknownAdditions=gitDiffAddedLines`。
- 上传目标仍沿用现有远程配置：`GIT_AI_REPORT_REMOTE_URL`，或 `GIT_AI_REPORT_REMOTE_ENDPOINT` + `GIT_AI_REPORT_REMOTE_PATH`。
- 鉴权仍沿用现有配置：`GIT_AI_REPORT_REMOTE_API_KEY`，以及 `GIT_AI_REPORT_REMOTE_USER_ID` 或 MCP 解析出的用户 ID。

**参数说明：**

| 参数 | 说明 | 默认值 / 行为 |
|------|------|---------------|
| `<commit>...` | 要上传的一个或多个 commit rev，支持 `HEAD`、`HEAD~2`、完整 SHA、短 SHA | 不传时默认只处理 `HEAD` |
| `--dry-run` | 只构造 payload 并打印摘要，不真正发请求 | 适合先确认 URL、source、summary 是否正确 |
| `--source <name>` | 写入 payload 的来源标记 | 默认 `manual` |
| `--ignore <pattern>` | 重新计算 stats 时忽略某类文件，可重复传入 | 例如 `--ignore '*.md' --ignore 'docs/**'` |

**常用命令：**

```bash
# 先预览当前 commit 会上传什么
git-ai upload-stats --dry-run

# 真正上传当前 commit
git-ai upload-stats

# 一次上传多个指定 commit
git-ai upload-stats HEAD~2 HEAD~1 HEAD

# 给这次补传打一个自定义来源标签
git-ai upload-stats --source review-backfill HEAD

# 重新计算时忽略文档和生成代码
git-ai upload-stats --dry-run --ignore '*.md' --ignore 'src/generated/**' HEAD
```

**结果判读：**

- 出现 `dry-run ... completed source=manual uploaded=0 dry_run=1 skipped=0 failed=0` 表示本地统计和配置可被正确解析，但这次没有真正发请求。
- 出现 `uploaded ... status=200` 表示对应 commit 已完成上传。
- 出现 `upload_stats_manual_missing_authorship_note` / `hasAuthorshipNote=false` 表示该 commit 本地没有 authorship note，命令按 metadata-only 口径上传未知归因记录。
- 出现 `upload_stats_wait_for_authorship_note_*` 表示 wrapper 兜底路径正在等待 daemon 写 note；如果等待结束仍没有 note，会继续走 metadata-only 上传。
- 最后一行 `completed source=... uploaded=<n> dry_run=<n> skipped=<n> failed=<n>` 是整次命令汇总；只要 `failed>0`，命令会以非 0 退出码结束，方便脚本或 CI 判断失败。

**何时用原生命令，何时用脚本：**

- 当前 commit 或少量指定 commit 的补传、复查、dry-run 预览：优先使用 `git-ai upload-stats`。
- 按日期范围、跨很多 commit、做历史批量回补：继续使用 `.specify/scripts/powershell/upload-ai-stats.ps1`。

### 2026-04-29：prompt 默认 notes、debug 本地归因日志与统计可靠性口径

**变更原因：**

`git_ai_prompt_stats.prompt_text` 是否能稳定落库，直接取决于 note 中是否保留 `prompts[].messages` 里的用户消息。旧默认 `prompt_storage=default` 会倾向 CAS / prompt store，并从 note 中清掉 messages，导致上传端拿不到正文。2026-05-09 的外部验证又进一步确认：即使 `prompt_storage=notes` 已经生效，只要 session/trace checkpoint 没有同步物化 `metadata.prompts`，或者 VS Code Copilot hook 仍然指向旧安装版 binary，真实 commit 仍会继续写出 `promptCount=0` 的 note。同时，现场排查归因问题时只靠 stderr 不够稳定，有些用户环境里看不到打印输出，需要本地文件日志兜底。

**本次代码级修改：**

| 文件 | 修改点 | 影响 |
|------|--------|------|
| `git-ai/src/commands/checkpoint_agent/presets/github_copilot/ide.rs` | GitHub Copilot native hook transcript 精确回查在匹配 `tool_use_id` / `toolCallId` 时兼容 VS Code 附加的 `__vscode-<digits>` 后缀，并保留真实 `chat_session_path` 供 session JSON model 提取使用 | 修复 VS Code hook `tool_input="..."` 时，明明存在成功 `apply_patch` / `create_file` transcript 记录却仍无法生成 `AiAgent` checkpoint、或因 `transcript_path` 覆盖 `chat_session_path` 导致 Copilot `selectedModel.identifier` 丢失的问题 |
| `git-ai/src/authorship/prompt_utils.rs` | post-commit refresh 现在会解析 GitHub Copilot transcript 的 event-stream 结构，回填 prompt text、transcript path 与 tool 元数据 | 修复 authorship note 已经保留 prompt hash，但 `messages` 仍为空、上传端提不出 `promptText` 的情况 |
| `git-ai/src/transcripts/model_extraction.rs` | 兼容 Copilot transcript 中 `data.modelId` / `data.modelID`、`selectedModel.identifier` 以及递归 model hint | 当 IDE transcript 或 session JSON 实际携带模型字段时，修复服务端 prompt/model 落库时 `model` 丢失或退回 `unknown` 的情况；若两个来源都没有模型字段，当前仍会退回 `unknown` |
| `git-ai/src/authorship/virtual_attribution.rs` | 对 `s_<session>::t_<trace>` 形式的 checkpoint 同时物化 `sessions` 与 `prompts`，统一 prompt metrics lookup，并在 authorship materialize 阶段追加 focused debug 事件 | 修复 note 里只剩 `sessions`、`upload-stats` 仍把 `promptCount/promptText` 记成 0 的第四类问题；`op-return-exchange` 最终提交 `17fa3b123...` 已验证恢复 |
| `git-ai/src/config.rs` | 新增 `DEFAULT_PROMPT_STORAGE="notes"`，未配置或配置非法时回退到 `notes`；`effective_prompt_storage` 的异常解析兜底也改为 `Notes` | 默认让 `prompts[].messages` 中的用户输入保留在 authorship note 中，支撑 `git_ai_prompt_stats.prompt_text` 稳定落库 |
| `git-ai/src/diagnostics.rs` | 新增统一 JSONL 诊断日志入口，设置 `GIT_AI_DEBUG` 后追加 `~/.git-ai/logs/debug.jsonl`，日志写入失败静默忽略 | debug 日志不影响 commit、stats 或上传主流程 |
| `git-ai/src/commands/checkpoint.rs` | 写入 `checkpoint_skipped`、`checkpoint_explicit_path_resolution`、`checkpoint_no_entries`、`checkpoint_attribution_decision`、`known_human_checkpoint_rejected`；`checkpoint_no_entries` / `checkpoint_attribution_decision` 增加逐文件 `fileDiagnostics[]`、上一条 `previousCheckpoint`、KnownHuman 元数据和诊断 hints | 即使用户看不到 stderr，也能在本地文件里确认本次 checkpoint 被判为 AI 还是人工，并解释每个候选文件为什么写入、跳过或没有进入候选集 |
| `git-ai/src/commands/checkpoint.rs` | 显式路径解析新增 `PreparedPathRole::WillEdit` / `PreparedPathRole::Edited` 的分流：`WillEdit` 会保留 clean 文本文件作为 checkpoint 基线，`Edited` 仍然要求 dirty 状态或显式 snapshot；同时新增单测覆盖这两个分支 | 修复“路径已经解析成功，却仍在 `checkpoint_explicit_path_resolution` 中以 `clean_file_without_dirty_snapshot` 被误杀”的第二类 Copilot/IDE 误归因问题，并保持 AI `edited_filepaths` 路径的过滤边界不变 |
| `git-ai/src/commands/checkpoint.rs` | 每个文件的 previous checkpoint 状态从“只保留最新一条”改为保留按时间顺序的历史；AI checkpoint 命中“最近一条是带 IDE 元数据的 `KnownHuman` 且内容相同”时，会忽略这条最近的 `KnownHuman`、退回到更早状态再生成 entry，并新增 `test_ai_checkpoint_reclaims_recent_known_human_file_capture` 回归测试 | 修复 `KnownHuman` 保存事件先落盘、AI checkpoint 紧随其后却被 `unchanged_from_previous_checkpoint` 吃掉的第三类误归因问题；`op-return-exchange` 的同步提交验证里，提交后 `stats/show/blame` 已恢复为 `github-copilot` |
| `git-ai/src/authorship/post_commit.rs` | 写入 authorship note、stats 计算成功和 stats 跳过事件 | 能解释 note 是否生成、prompt 是否进入 note、以及 stats 缺失是否导致自动上传跳过 |
| `git-ai/src/integration/upload_stats.rs` | 写入上传跳过、ready、started、failed、succeeded 事件，记录 URL 来源、用户 ID/API Key 状态、payload 摘要和 HTTP 结果 | 能从本地日志判断统计未上传是配置、payload、stats 缺失、网络还是服务端响应问题 |
| `git-ai/docs/design-doc/git-ai看板方案.md` | 补充默认值、debug 日志路径、统计字段可靠性和 Phase 2 代码修改项 | 看板方案与当前代码状态保持一致 |

**debug 日志内容边界：**

只记录 `CheckpointKind`、AI/人工决策、repo 路径、base/commit sha、文件路径样例、文件数、行数、显式路径过滤原因、逐文件跳过原因、上一条 checkpoint 的 kind/时间/agent 摘要、KnownHuman editor/extension 版本、工具、模型、是否有 transcript、stats 汇总、URL 来源、是否配置 API Key / X-USER-ID、HTTP 状态和错误摘要等元数据；不记录代码内容，不记录 prompt 正文，也不记录完整 prompt id。

**统计可靠性结论：**

看板主指标优先使用 Git 原生提交字段、`hasAuthorshipNote`、`gitDiffAddedLines` / `gitDiffDeletedLines`、`stats.files[]`、`aiAdditions`、`humanAdditions` 和 `unknownAdditions`。其中 `unknownAdditions` 只能解释为“未归因”，不能自动解释成人工。`promptText` 只在 `notes` 模式下稳定；`timeWaitingForAi`、`totalAiAdditions`、`totalAiDeletions` 更适合作辅助参考。

### 2026-04-29：补充 daemon lock 自恢复与现场处置办法

**变更原因：**

现场出现过 `failed to connect to git-ai background service: daemon startup blocked: lock held at ...daemon.lock`。该错误发生在本机 async daemon 启动 / 连接阶段，早于远程 API 上传，不应被误判为服务端上传接口失败。

**本次补充的处理点：**

| 项目 | 处理方式 |
|------|----------|
| 快速现场恢复 | 优先执行 `git-ai bg restart --hard` |
| Windows 手动兜底 | 从 `$HOME\.git-ai\internal\daemon\daemon.pid.json` 读取 pid 后执行 `taskkill /F /T /PID <pid>` |
| 禁止动作 | 不建议手动删除 `daemon.lock`，因为 Windows 下它代表仍有进程持有独占句柄 |
| 两类报错含义 | 旧版 `daemon startup blocked: lock held` 表示发现锁后直接失败；新版 `failed to recover unhealthy daemon pid ... taskkill ... 拒绝访问` 表示已经尝试回收旧 daemon，但 Windows 权限拒绝结束该进程 |
| 代码级自恢复 | `ensure_daemon_running` 遇到锁占用时先等待已有 daemon 完成启动；若 socket 持续不可用且 pid 可读，自动强制回收旧 daemon 并重启；若回收被拒绝，则直接保留 blocked startup 错误并拒绝激活 replacement runtime，避免把一个坏 daemon 扩散成多套 `bg run` runtime |

**当前效果：**

正常的 daemon 启动竞争会被等待吸收；daemon 进程存活但 socket 异常的情况会自动重启恢复。若 Windows 拒绝结束旧 daemon，当前实现不会再换一套 runtime / pipe 继续服务，而是明确保留 blocked startup 错误，阻断新的 daemon 扩散。这样做的取舍是：宁可把异常保留在一个明确错误上，也不再把一个坏 daemon 扩散成多套 runtime 后共同争用全局 `upload_activity.lock`。只有在 pid 元数据缺失、锁长期不释放、或需要现场清理旧残留进程时，才需要人工介入。

### 2026-04-27：安装链路补充“自动更新到目标最新版本”与“自定义 GitHub 仓库来源”

**变更原因：**

原方案只把重点放在“Spec Kit 初始化时自动安装 git-ai”，但当前真实需求已经变成：**安装或更新 Spec Kit 时，都必须把 git-ai 同步安装 / 更新到目标最新版本。** 此外，git-ai 的来源不再一定是官方仓库，可能要切到团队自己的 GitHub fork。

**本次补充的设计点：**

| 项目 | 新约束 |
|------|--------|
| Spec Kit 调用 post-init | `specify init` / `specify init --here --force` 都要以 force-update 语义执行 post-init，不能继续走“已安装就跳过”的旧逻辑 |
| 自定义 GitHub 仓库来源 | installer 需要支持运行时覆盖 `GIT_AI_GITHUB_REPO`，必要时再配 `GIT_AI_RELEASE_TAG` |
| 版本来源说明 | branch URL 只能决定 installer 脚本从哪里下载，真正被安装的 binary 仍来自 release asset 或 `GIT_AI_LOCAL_BINARY` |
| 需求确认清单 | 接入自定义仓库前，必须先明确 repo、installer URL、release/tag 规则、binary asset 命名和发布位置 |

**当前结论：**

如果你的 GitHub 发布现在还是跟着测试分支走，那么直接写 `GIT_AI_RELEASE_TAG=latest` 只会继续安装“测试分支对应的最新 release”。如果要让 Spec Kit 安装 / 更新到 `feature/mcp-x-user-id-hook-payload` 对应的版本，你需要先把这条分支产出成 release / prerelease / tag，或者在验证阶段改用 `GIT_AI_LOCAL_BINARY`。

**实现补充（2026-05-08，commit 成功后后台自更新）：**

需求 1 里“自动更新到目标最新版本”原先更多落在安装链路和 `specify init` 上，但当前实现已经进一步前移到日常 Git 工作流：`git-ai` 在 `git commit` 成功后就会调度一次后台升级检查，而不再只等 `push`。这样做的核心目的是把“版本太旧”的发现和修复时机提前到开发者最常发生的动作上，同时继续保证提交主路径不被下载 / 安装阻塞。

这里复用的是现有 `git-ai upgrade --background` 路径，而不是另起一套 commit 专用下载器，因此它天然继承以下边界：

- 只在 commit 成功后触发，失败 commit、非 commit 命令和只读命令不会误触发。
- 后台检查 / 安装仍受 `disable_version_checks`、`disable_auto_updates`、`update_channel`、24 小时检查间隔和后台进程节流约束。
- 版本发现直接读取 `configured_github_repo()` 对应的 GitHub releases，安装器优先来自目标 tag 的 release asset，缺失时回退到同仓库 raw installer，而不是在本机直接拉源码构建二进制。
- Windows 场景继续通过 detached 的后台升级过程完成文件替换，避免当前运行中的 `git` / `git-ai` 父子进程锁住二进制文件。

### 2026-04-24：补充 git-ai 原生自动上传实现

**变更原因：**

原文档主要围绕 Speckit 的 `upload-ai-stats.ps1` 和 Code Review 批量上传展开，但当前实际需求已经变成：**在 `git-ai` 生成 commit 统计结果时，直接用同一套接口和参数即时上传。** 如果文档继续只写脚本方案，会和真实代码状态脱节。

**本次补充的实现点：**

| 项目 | 已落地内容 |
|------|-----------|
| 原生上传模块 | 新增 `git-ai/src/integration/upload_stats.rs`，负责组装与脚本一致的 JSON payload 并发起 HTTP POST |
| 上传接入点 | `git-ai/src/authorship/post_commit.rs` 在写入 authorship note、计算出 `CommitStats` 后调用 `maybe_upload_after_commit(...)` |
| 配置开关 | `git-ai/src/feature_flags.rs` 新增 `auto_upload_ai_stats`，默认开启；如需显式覆盖，使用 `GIT_AI_AUTO_UPLOAD_AI_STATS=true/false` |
| 身份头复用 | 继续使用 `git-ai/src/integration/ide_mcp.rs` 的 `resolve_x_user_id(...)` 解析 `X-USER-ID` |
| 模块注册 | `git-ai/src/integration/mod.rs` 导出 `upload_stats` |

**当前代码行为：**

1. commit 成功后先写 authorship note。
2. 若当前 commit 不是 merge / 不是 expensive-skip，`git-ai` 会拿到内存中的 `CommitStats`。
3. 若 feature flag 开启，就直接把同一份 commit 数据组装成单 commit batch 上传。
4. headers、逐文件统计、时间格式、repoUrl / branch / projectName 推导逻辑与脚本方案保持兼容。
5. 上传失败不会让 commit 失败。

**已完成的代码级验证：**

| 验证项 | 结果 |
|------|------|
| `cargo build --features test-support` | ✅ 通过 |
| `cargo test --features test-support --lib integration::upload_stats` | ✅ 13 个测试全部通过 |
| `cargo test --features test-support --lib feature_flags` | ✅ 10 个测试全部通过 |
| `cargo clippy --features test-support --lib -- -D warnings` | ✅ 通过 |
| `cargo fmt` | ✅ 通过 |

**当前仍需注意：**

- 文档里的这次更新是“代码级实现已落地、编译和单测已验证”；真实业务仓库联调和远程接口实测，仍需要在目标仓库环境里再做一次 end-to-end 验证。

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
