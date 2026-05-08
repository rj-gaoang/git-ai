# feature_20260429_gaoang 合并 upstream/main 冲突分析

生成时间：2026-05-08

本文只给出冲突处理建议，不执行合并、不改业务代码。分析方式是在临时 worktree 中执行：

```powershell
git worktree add --detach D:\git-ai-main\.merge-analysis-git-ai-0920aa64 feature_20260429_gaoang
git -C D:\git-ai-main\.merge-analysis-git-ai-0920aa64 merge --no-commit --no-ff upstream/main
```

本次用于分析的两个端点：

| 分支 | commit |
|------|--------|
| `feature_20260429_gaoang` | `45a436bfa454b02c926f05f9bfac4a48598b3ccb` |
| `upstream/main` | `4f2f0a9eb0829ca83397ffd93ec174ceda7b83e6` |

## 总体结论

这次不是普通小冲突。`upstream/main` 已经把 checkpoint / agent preset / daemon / transcript 流程做了大规模重构：

- 旧的 `src/commands/checkpoint.rs` 在 upstream 中被删除，核心逻辑迁移到 `src/daemon/checkpoint.rs`。
- 旧的 `src/commands/checkpoint_agent/agent_presets.rs` 在 upstream 中被删除，拆成了 `src/commands/checkpoint_agent/presets/*`。
- upstream 引入了 sessions / trace_id / transcript streaming / transcript sweep 等新架构。

因此建议采用这个总策略：

1. **结构以 upstream/main 为基线**：不要把旧的 `checkpoint.rs` 和旧的单体 `agent_presets.rs` 强行保留下来。
2. **功能以 feature 分支为保留清单**：本分支已经修复的一批真实问题不能丢，应该移植到 upstream 的新模块位置。
3. **官方已解决且更靠前的 bug，优先用官方方案**：尤其是 KnownHuman 抢占 AI 的竞态，upstream 已经在 daemon checkpoint 层做 pending AI edit suppression，比本分支旧文件里的事后 reclaim 更合适。
4. **官方没有的本地能力必须保留**：原生上传、`upload-stats`、`auto_upload_ai_stats`、用户 prompt-only 上传、Copilot `__vscode-*` 后缀兼容、空白行归因、中文路径逐文件统计等。

## 冲突文件清单

| 文件 | 冲突类型 | 建议 |
|------|----------|------|
| `Cargo.toml` | 内容冲突 | 手工合并，保留 fork 版本号和本地新增依赖，同时加入 upstream 新依赖 |
| `Cargo.lock` | 内容冲突 | 不手工逐行解，代码合并后重新生成 |
| `flake.nix` | 内容冲突 | 手工合并，版本号跟 `Cargo.toml` 对齐 |
| `install.ps1` | 内容冲突 | 手工合并，保留本地 Windows 进程清理修复 |
| `src/authorship/post_commit.rs` | 内容冲突 | 手工合并，upstream sessions + feature 上传/prompt 过滤都保留 |
| `src/authorship/secrets.rs` | 内容冲突 | 手工合并，保留本地 prompt message 过滤函数 |
| `src/authorship/stats.rs` | 内容冲突 | 手工合并，保留 upstream session 统计，同时移植本地空白行归因 |
| `src/commands/checkpoint.rs` | upstream 删除 / feature 修改 | 删除旧文件，移植必要修复到 `src/daemon/checkpoint.rs` |
| `src/commands/checkpoint_agent/agent_presets.rs` | upstream 删除 / feature 修改 | 删除旧文件，移植必要修复到 `presets/*` |
| `src/commands/daemon.rs` | 内容冲突 | 手工合并，保留 upstream 测试防进程风暴和 feature daemon recovery |
| `src/commands/install_hooks.rs` | 内容冲突 | 手工合并，保留 async 关闭时清理 trace2 的本地逻辑 |
| `src/config.rs` | 内容冲突 | 手工合并，保留 upstream 新探测路径，同时保留本地更精确 shim 判断 |
| `src/feature_flags.rs` | 内容冲突 | 手工合并，保留两边所有 feature flags |
| `src/git/refs.rs` | 内容冲突 | 主要是测试冲突，保留本地 authorship note 兼容测试 |
| `tests/integration/performance.rs` | 内容冲突 | 手工合并 FeatureFlags 初始化字段 |
| `src/authorship/snapshots/*.snap` | 快照冲突 | 不直接选边，代码合并后重新跑 insta 审核 |

## 精确保留清单（函数 / 字段 / 调用点级别）

这一节回答的不是“保留哪个文件”，而是“旧分支里到底哪一段代码要留下来，哪一段不要整段带过去”。

### A. `src/commands/checkpoint.rs`：只迁移边界行为和诊断，不保留旧主流程

**不要整段保留的旧逻辑：**

- `const AI_RECLAIM_RECENT_KNOWN_HUMAN_SECS: u64 = 10;`
- `fn should_ignore_recent_known_human_for_ai_checkpoint(...) -> bool`
- `fn select_previous_state_for_checkpoint(...) -> Option<PreviousFileState>` 中基于“最近 KnownHuman 回退到倒数第二个 previous state”的整段 reclaim 逻辑

原因：这三段是“AI 事后 reclaim 已写入的 KnownHuman checkpoint”的旧方案。合并后应以 upstream `src/daemon/checkpoint.rs` 里更早触发的 KnownHuman suppression 为主，不应再保留旧 reclaim 主流程。

**必须迁移保留的旧逻辑：**

- `PreparedPathRole::WillEdit` 对 clean 文件的保留分支：

```rust
let preserve_unchanged_explicit_paths = explicit_path_role == PreparedPathRole::WillEdit;

if status_entry.is_none()
  && explicit_dirty_content.is_none()
  && !preserve_unchanged_explicit_paths
{
  // reason = "clean_file_without_dirty_snapshot"
}
```

这段不能丢。它的作用是避免 `WillEdit` 显式路径在“当前文件还没脏、但 IDE 已经告诉 git-ai 这个文件即将被编辑”时被错误丢弃。迁移时应放进 upstream 新 checkpoint 路径解析/显式文件筛选逻辑里，而不是继续留在已删除的旧文件中。

- KnownHuman metadata 赋值块：

```rust
} else if kind == CheckpointKind::KnownHuman
  && let Some(agent_run) = &agent_run_result
  && let Some(meta) = &agent_run.agent_metadata
{
  let editor = meta.get("kh_editor").cloned().unwrap_or_default();
  let editor_version = meta.get("kh_editor_version").cloned().unwrap_or_default();
  let extension_version = meta
    .get("kh_extension_version")
    .cloned()
    .unwrap_or_default();
  if !editor.is_empty() {
    use crate::authorship::working_log::KnownHumanMetadata;
    checkpoint.known_human_metadata = Some(KnownHumanMetadata {
      editor,
      editor_version,
      extension_version,
    });
  }
}
```

这段要迁到 upstream `src/daemon/checkpoint.rs` 创建/落盘 checkpoint 的位置，因为后续 debug 和误归因排查都依赖 `known_human_metadata`。

- 诊断函数和输出字段：
  - `fn file_checkpoint_root_cause_hint(...)`
  - `fn previous_checkpoint_diagnostic(...)`
  - `fn previous_checkpoint_debug_json(...)`

尤其是 `previous_checkpoint_debug_json(...)` 里的：

```rust
"knownHumanMetadata": {
  "editor": previous_checkpoint.known_human_editor.as_deref(),
  "editorVersion": previous_checkpoint.known_human_editor_version.as_deref(),
  "extensionVersion": previous_checkpoint.known_human_extension_version.as_deref(),
}
```

这段建议保留，因为它直接决定你后面 debug.jsonl / 诊断输出里能不能看到 VS Code / 插件版本等信息。

### B. `src/commands/checkpoint_agent/agent_presets.rs`：只迁移 Copilot ID 兼容 helper，不保留旧单体 preset 文件

**必须迁移保留的旧逻辑：**

- `fn strip_vscode_tool_call_suffix(id: &str) -> &str`
- `fn copilot_tool_call_id_matches(candidate_id: &str, tool_use_id: &str) -> bool`
- `fn extract_filepaths_from_matching_copilot_tool_call(...) -> Option<Vec<String>>` 里这一句比较：

```rust
if !Self::copilot_tool_call_id_matches(id, tool_use_id) {
  return None;
}
```

这三段要迁到 upstream `src/commands/checkpoint_agent/presets/github_copilot/ide.rs` 或相邻 helper 模块，不能再留在被 upstream 删除的旧 `agent_presets.rs`。

迁移时需要保留的核心行为是：

- `tool_use_id="abc"` 能匹配 `candidate_id="abc__vscode-12345"`
- `tool_use_id="abc__vscode-12345"` 能匹配 `candidate_id="abc"`
- `abc__vscode-1` 不能误匹配 `abc__vscode-2`

**建议连同测试一起迁移：**

- `test_copilot_tool_call_id_matches_keeps_distinct_vscode_suffix_ids_distinct`
- 与 `strip_vscode_tool_call_suffix` / `copilot_tool_call_id_matches` 相邻的同组测试

### C. `src/authorship/post_commit.rs`：保留 prompt 过滤和上传调用，但以 upstream sessions 骨架为主

**必须保留的 import：**

```rust
use crate::authorship::secrets::{
  redact_secrets_from_prompts, retain_user_prompt_messages, strip_prompt_messages,
};
```

**必须保留的 prompt 过滤调用点：**

```rust
retain_user_prompt_messages(&mut authorship_log.metadata.prompts);
```

这句必须发生在真正写 note / CAS / local storage 之前。否则 assistant/thinking/tool_use 重新进入 note 或上传 payload。

**必须保留的 `PromptStorageMode` 分支行为：**

- `PromptStorageMode::Local` 分支里的 `strip_prompt_messages(&mut authorship_log.metadata.prompts);`
- `PromptStorageMode::Notes` 分支里的 `redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);`
- `PromptStorageMode::Default` 分支里“CAS 前先 `redact_secrets_from_prompts(...)`，失败或不上传则 `strip_prompt_messages(...)`”的完整行为

**必须保留的上传调用点：**

```rust
crate::integration::upload_stats::maybe_upload_after_commit(
  repo,
  &commit_sha,
  &authorship_log,
  stats.as_ref(),
);
```

以及上传前的 debug event：

```rust
"post_commit_upload_dispatch_requested"
```

这两段要保留，否则你 fork 的自动上传链路会直接断掉。

**不要删掉 upstream 已有的 sessions 定制属性注入：**

- upstream 已经对 `authorship_log.metadata.sessions.values_mut()` 注入 `custom_attributes`
- 合并时应变成“双写”：`prompts.values_mut()` 继续保留，本分支的 prompt-only 过滤继续保留，upstream 的 `sessions.values_mut()` 也不能丢

### D. `src/authorship/secrets.rs`：这三个函数要原样保留

这三个函数是可以直接保留的，不是只保留思想：

- `pub fn redact_secrets_from_prompts(prompts: &mut BTreeMap<String, PromptRecord>) -> usize`
- `pub fn retain_user_prompt_messages(prompts: &mut BTreeMap<String, PromptRecord>)`
- `pub fn strip_prompt_messages(prompts: &mut BTreeMap<String, PromptRecord>)`

尤其是 `retain_user_prompt_messages(...)` 里这一句应原样保留：

```rust
record
  .messages
  .retain(|message| matches!(message, Message::User { .. }));
```

测试也应保留：

- `retain_user_prompt_messages_drops_non_user_entries`

### E. `src/authorship/stats.rs`：保留“逐文件统计 + 空白行归并”，不要把旧 owner 解析整段带过去

**必须保留的函数：**

- `pub(crate) fn accepted_lines_from_attestations_by_file(...) -> BTreeMap<String, FileAcceptedLineStats>`
- `fn infer_whitespace_only_added_lines(...)`
- `fn line_range_overlap_start_index(...)`
- `fn line_range_overlap_end_index(...)`
- `fn add_owner_counts(...)`
- `fn is_whitespace_only_line(...)`

其中真正不能丢的是这句调用：

```rust
infer_whitespace_only_added_lines(
  repo,
  commit_sha,
  &file_attestation.file_path,
  added_lines,
  &mut direct_owners,
  &mut file_stats,
);
```

它决定“新增空白行是否归给左右相邻的 AI / KnownHuman block”。

**不要整段保留的旧逻辑：**

- 旧 `fn owner_for_entry(log: &AuthorshipLog, entry_hash: &str) -> AddedLineOwner`

原因：upstream 新 `accepted_lines_from_attestations(...)` 已经能识别：

- `h_` 前缀的人类 entry
- `s_` 前缀的 session entry，并从 `log.metadata.sessions` 解析 tool/model

所以正确做法不是把旧 `owner_for_entry` 整段覆盖 upstream，而是：

- 以 upstream 的 `accepted_lines_from_attestations(...)` / session-aware owner 解析为主
- 把本分支的 `accepted_lines_from_attestations_by_file(...)` 和 `infer_whitespace_only_added_lines(...)` 嵌到 upstream 口径上

### F. `src/integration/upload_stats.rs` 和 `src/commands/git_ai_handlers.rs`：这几个入口要完整保留

这部分 upstream 没有等价实现，因此不是“挑一段迁移”，而是这些入口函数必须完整保留：

- `pub fn maybe_upload_after_commit(...)`
- `pub fn upload_local_commit_stats(...)`
- `fn build_payload(...)`
- `fn build_payload_with_source(...)`
- `fn build_prompt_stats(...)`
- `fn build_file_stats(...)`

命令路由也必须保留：

```rust
"upload-stats" | "upload-ai-stats" => {
  handle_upload_stats(&args[1..]);
}
```

如果这一段丢掉，即使 `upload_stats.rs` 还在，CLI 也无法主动上传。

### G. `src/commands/daemon.rs`：保留 daemon recovery / replacement runtime 这几段

**必须保留的函数：**

- `fn recover_blocked_daemon_startup(...) -> Result<Option<DaemonConfig>, String>`
- `fn start_replacement_daemon_runtime(...) -> Result<DaemonConfig, String>`
- `fn hard_kill_daemon_pid(...)` 的 Windows 实现（`taskkill /F /T /PID ...`）

**必须保留的 fallback 调用点：**

- `handle_restart(...)` 里的：

```rust
eprintln!(
  "[git-ai] warning: hard restart could not kill existing daemon: {}; starting replacement runtime",
  error
);
return ensure_daemon_running(daemon_startup_timeout()).map(|_| ());
```

- `restart_daemon(...)` 里的：

```rust
eprintln!(
  "[git-ai] warning: failed to stop existing background service before restart: {}; starting replacement runtime",
  error
);
```

这几段的价值不在“日志文案”，而在于“旧 daemon 卡住时，不让 install/restart 永远失败，而是切到 replacement runtime”。

### H. `src/commands/install_hooks.rs`：保留 async 关闭时清理 trace2 的分支

**必须保留的分支：**

```rust
if !runtime_config.feature_flags().async_mode {
  if !dry_run {
    let _ = remove_global_git_config_section("trace2");
  }
  return Ok(());
}
```

它位于 `fn maybe_configure_async_mode_daemon_trace2(dry_run: bool)` 中。

这段必须保留，因为它保证“把 async_mode 关掉以后，历史遗留的 `trace2.eventTarget` 也会被清掉”。否则用户以为已经切回同步模式，实际上 Git trace2 还可能继续打到 daemon。

**还要保留这个 helper 的可调用性：**

- `pub(crate) fn configure_async_mode_daemon_trace2_for_config(daemon_config: &DaemonConfig)`

因为本分支 `start_replacement_daemon_runtime(...)` 会直接调用它，把 Git trace2 改指向 replacement runtime。

### I. `src/config.rs`：保留“复制型 shim”识别，不覆盖 upstream 更完整的 Git 查找

**必须保留的 helper：**

- `fn files_have_same_contents(a: &Path, b: &Path) -> bool`

**必须保留的判断分支：**

```rust
if sibling.exists() {
  if same_file(path, &sibling) {
    return true;
  }

  if files_have_same_contents(path, &sibling) {
    return true;
  }
}
```

这段位于 `fn path_is_git_ai_binary(path: &Path) -> bool` 中，作用是识别 Windows installer 通过“复制 `git-ai.exe` 为 `git.exe`”形成的 shim。这个场景下 `same_file(...)` 不成立，但字节内容一致，因此不能丢 `files_have_same_contents(...)`。

**不要被本分支覆盖掉的 upstream 搜索增强：**

- `LOCALAPPDATA` Git 候选路径
- `where.exe git.exe` fallback

正确方式是两边叠加：保留 upstream 更完整的 Git 发现逻辑，再保留本分支“复制型 shim 识别”这一小段。

### J. `src/feature_flags.rs`：这两个 flag 定义和所有初始化位都要保留

**必须保留的定义：**

```rust
format_only_attribution_passthrough: format_passthrough, debug = true, release = true,
auto_upload_ai_stats: auto_upload_ai_stats, debug = true, release = true,
```

**必须同步保留的初始化字段：**

- `FeatureFlags { ... format_only_attribution_passthrough: true, auto_upload_ai_stats: false, ... }`
- 所有测试构造体、尤其 `tests/integration/performance.rs` 里的 `FeatureFlags` 初始化都要补齐这两个字段

### K. `install.ps1`：保留 `$processIds`，不要回退成 `$pids`

**必须保留的代码段：**

```powershell
$processIds = @($processes | Sort-Object ProcessId -Unique | Select-Object -ExpandProperty ProcessId)
Write-Warning ("Stopping lingering git-ai processes: {0}" -f ($processIds -join ', '))

foreach ($processId in $processIds) {
  try {
    Stop-Process -Id $processId -Force -ErrorAction Stop
  } catch { }
}
```

不要把这里改回 `$pids`。`$PID` 是 PowerShell 预留自动变量，使用过于接近的命名会再次引入安装脚本兼容性问题。

## 逐模块建议

### 1. Checkpoint 主流程

冲突文件：

- `src/commands/checkpoint.rs`
- upstream 新位置：`src/daemon/checkpoint.rs`

本分支做过的关键修复：

- `KnownHuman` 抢先写入后，AI checkpoint 因为内容相同被判定为 `unchanged_from_previous_checkpoint` 的事后 reclaim。
- `WillEdit` 显式路径在 clean 文本文件上不应被 `clean_file_without_dirty_snapshot` 误杀。
- `KnownHuman` 的 pre-save / post-save 路径选择。
- 更详细的 checkpoint debug 诊断。

upstream/main 的相关变化：

- 删除旧 `src/commands/checkpoint.rs`。
- 新增 `src/daemon/checkpoint.rs`。
- 已有 `KnownHuman` 抑制逻辑，位置在 daemon checkpoint 处理层。
- 上游相关修复提交包括：
  - `b1a0ad1b fix: suppress KnownHuman checkpoints during in-flight AI edits`
  - `05ac1aae refactor: remove is_ai_pre_edit field, use path_role + agent_id instead`
  - `5d6ea452 fix: filter per-file instead of suppressing entire multi-file KnownHuman checkpoint`
  - `439ba431 fix: recompute checkpoint_file_paths after KnownHuman filtering`
  - `984c91fb fix: canonicalize paths in pending AI edit tracking`

建议处理：

- **不要保留旧 `src/commands/checkpoint.rs`。** 接受 upstream 删除。
- KnownHuman 抢占 AI 的核心问题，优先采用 upstream 的 daemon-side pending AI edit suppression。它比本分支的“AI 后到时回退上一条 KnownHuman”更早拦截，语义更干净。
- 本分支里的 debug 诊断、`WillEdit` clean 文件保留逻辑、KnownHuman metadata 输出，如果 upstream 新实现没有等价能力，应移植到 `src/daemon/checkpoint.rs`。
- 本分支测试 `test_ai_checkpoint_reclaims_recent_known_human_file_capture` 不建议原样保留断言实现细节，应改写成验证 upstream 新架构的行为：KnownHuman 保存事件被抑制，最终 AI attribution 正确。

为什么这么处理：

旧文件已经被 upstream 架构替换。强行保留旧文件会绕过 upstream 的 daemon checkpoint rewrite，后续与 transcript worker / sessions / sweep 体系不兼容。官方方案已经解决同一类 bug，而且位置更靠前，所以应以官方方案为主，只补本地缺失的诊断和边界测试。

### 2. Agent preset / GitHub Copilot hook

冲突文件：

- `src/commands/checkpoint_agent/agent_presets.rs`
- upstream 新位置：`src/commands/checkpoint_agent/presets/*`
- Copilot 相关新位置：`src/commands/checkpoint_agent/presets/github_copilot/ide.rs`、`mod.rs`

本分支做过的关键修复：

- GitHub Copilot VS Code native hook 的 `tool_use_id` 可能带 `__vscode-<digits>` 后缀。
- 本分支通过 `strip_vscode_tool_call_suffix` / `copilot_tool_call_id_matches` 兼容“一侧带后缀、一侧不带后缀”的同一次 tool call。
- 同时避免把两个不同 `__vscode-*` 后缀实例误合并。

upstream/main 的相关变化：

- 删除单体 `agent_presets.rs`。
- 拆分为 per-agent preset 文件。
- upstream 的 `github_copilot/ide.rs` 已有 native hook 和 transcript 路径识别，但搜索未发现 `strip_vscode`、`copilot_tool_call_id_matches` 或等价 `__vscode-*` 后缀兼容。

建议处理：

- **不要保留旧 `agent_presets.rs`。** 接受 upstream 拆分。
- 将本分支的 `__vscode-*` 后缀兼容函数和测试移植到 `src/commands/checkpoint_agent/presets/github_copilot/ide.rs` 或 `mod.rs`。
- 移植时保持 upstream 的 `ParsedHookEvent` / `PresetContext` / `TranscriptSource` 新结构，不回退旧 `AgentRunResult`。

为什么这么处理：

upstream 已完成结构性重构，但没有覆盖这个具体 bug。这个 bug 与 2026-05-03 的真实误归因直接相关，丢掉会导致 Copilot native hook 在 `tool_input="..."` 场景下再次无法从 transcript 精确回查路径。

### 3. 原生上传和 post-commit 链路

冲突文件：

- `src/authorship/post_commit.rs`
- `src/feature_flags.rs`
- 相关非冲突新增：`src/integration/upload_stats.rs`、`src/integration/mod.rs`、`src/commands/git_ai_handlers.rs`

本分支做过的关键功能：

- `feature_flags.auto_upload_ai_stats` 默认开启。
- `post_commit` 生成 authorship note / stats 后触发 `maybe_upload_after_commit(...)`。
- 新增 `git-ai upload-stats` / `upload-ai-stats` 主动上传命令。
- 上传链路记录详细 `debug.jsonl` 事件。
- 上传 HTTP 200 后继续检查服务端 JSON body 的 `code/msg`，避免业务失败被误判成功。

upstream/main 的相关变化：

- 没有 `src/integration/upload_stats.rs`。
- 没有 `auto_upload_ai_stats`。
- 没有本分支这套自有 API 上传逻辑。
- 但 upstream 的 `post_commit.rs` 已引入 sessions metadata，且会给 `metadata.sessions` 注入 custom attributes。

建议处理：

- `post_commit.rs` 以 upstream sessions 结构为基线，保留 upstream 对 `metadata.sessions` 的 custom attributes 注入。
- 同时保留本分支：prompt 刷新、prompt 过滤、stats debug、`maybe_upload_after_commit(...)` 调用、stats skip debug。
- `src/integration/upload_stats.rs` 必须保留，并按 upstream 新 stats/session 数据结构适配。
- `feature_flags.rs` 必须同时包含 upstream 的 `transcript_streaming` / `transcript_sweep` 和本分支的 `auto_upload_ai_stats` / `format_passthrough`。

为什么这么处理：

上传能力是本 fork 的核心二开能力，upstream 没有等价实现，不能被上游覆盖。反过来，upstream 的 sessions 是新架构基础，不能为了上传保留旧 prompt-only 结构。正确方式是二者合并。

### 4. Prompt 隐私与 secret 处理

冲突文件：

- `src/authorship/post_commit.rs`
- `src/authorship/secrets.rs`
- `src/config.rs`

本分支做过的关键修复：

- `retain_user_prompt_messages(...)`：只保留 `Message::User`，不保存/上传 assistant、thinking、plan、tool_use 内容。
- `redact_secrets_from_prompts(...)`：持久化或上传前做 secret 脱敏。
- `prompt_storage` 默认值改为 `notes`，方便当前自有上传链路稳定落库 `promptText`。

upstream/main 的相关变化：

- upstream 仍然有 `prompt_storage` / `default_prompt_storage` / `effective_prompt_storage`，但默认仍是 `default`。
- upstream 引入 `sessions` 元数据，但未发现本分支 `retain_user_prompt_messages` 的等价实现。

建议处理：

- 保留 upstream 的 prompt storage 配置结构和 sessions 支持。
- 保留本分支 `retain_user_prompt_messages(...)`，并在 `post_commit` 写 note / CAS / local 前调用。
- 是否继续默认 `prompt_storage=notes` 属于产品决策：如果当前看板仍要求 prompt_text 快速落库，则保留本分支默认；如果以后要收紧隐私，可改回 upstream default 并补 CAS/SQLite 回填链路。

为什么这么处理：

upstream 的 sessions 不等于隐私收敛。当前本分支已经明确只保留用户输入，这是隐私边界修复，不能因为合并 upstream 而退回保存完整 transcript。

### 5. Commit stats / 空白行归因 / 文件级统计

冲突文件：

- `src/authorship/stats.rs`
- 相关非冲突：`src/integration/upload_stats.rs`
- 快照：`src/authorship/snapshots/*.snap`

本分支做过的关键修复：

- `infer_whitespace_only_added_lines(...)`：新增空白行如果紧邻已归因块，应归并到相邻 KnownHuman / AI，而不是掉进 `unknownAdditions`。
- `accepted_lines_from_attestations_by_file(...)`：为上传 `stats.files[]` 提供逐文件 commit-local 归因明细。
- 中文/非 ASCII 文件名统计通过 `core.quotepath=false` 避免路径转义不匹配。

upstream/main 的相关变化：

- upstream 的 `accepted_lines_from_attestations(...)` 已支持 `s_` session attribution，能从 `metadata.sessions` 取 tool/model。
- upstream 未发现 `infer_whitespace_only_added_lines` 等价实现。

建议处理：

- 以 upstream 的 session-aware `accepted_lines_from_attestations` 为基线。
- 移植本分支的逐文件统计和空白行邻接归并逻辑。
- 上传侧 `stats.files[]` 必须继续使用 commit-local 语义，并保留 `core.quotepath=false`。
- 快照冲突不要直接选 ours/theirs，合并代码后运行 `cargo insta review` 审核再接受。

为什么这么处理：

upstream 解决的是新 session 口径，本分支解决的是统计准确性和看板逐文件口径，两者不冲突，应该叠加。

### 6. Daemon 控制与后台服务恢复

冲突文件：

- `src/commands/daemon.rs`

本分支做过的关键修复：

- daemon 启动被旧 lock 阻塞时，等待、读取 pid、强杀旧进程或启用 replacement runtime。
- Windows 下 replacement runtime 会重指向 trace2 pipe，避免继续被旧 daemon lock 卡住。

upstream/main 的相关变化：

- 显式 `bg start` / `bg restart` 和 wrapper auto-spawn 的职责拆分更清楚。
- test-support 构建中禁止自动拉 daemon，避免并行测试进程风暴。
- `bg tail` 行为有官方修复：默认打印后退出，需要 follow 时显式 `-f/--follow`。

建议处理：

- 保留 upstream 的 test-support auto-spawn guard 和 `bg tail` 新语义。
- 移植本分支 blocked daemon recovery / replacement runtime。
- 注意不要让 recovery 逻辑绕过 upstream 的测试构建保护。

为什么这么处理：

两边修的是不同问题：upstream 解决测试/命令语义和 daemon storm，本分支解决用户机器旧 daemon 卡死。两者都重要，不能简单选边。

### 7. install-hooks / trace2 配置

冲突文件：

- `src/commands/install_hooks.rs`

本分支做过的关键修复：

- 当 `feature_flags.async_mode=false` 时，清理之前写入的 `trace2` 配置，避免用户切回同步模式后仍然走 daemon trace2。

upstream/main 的相关变化：

- trace2 / daemon 配置路径与新 daemon 体系继续演进。

建议处理：

- 保留 upstream 的新 trace2 配置方式。
- 在进入配置前保留本分支 async 关闭时清理 trace2 的逻辑。

为什么这么处理：

这个逻辑直接影响“临时改同步模式排查问题”是否真的生效。若丢掉，用户以为关了 async，实际 Git trace2 仍可能打到 daemon。

### 8. Windows installer

冲突文件：

- `install.ps1`

冲突点：

- 本分支使用 `$processIds` 遍历 `Get-GitAiManagedProcesses` 的结果。
- upstream 冲突块使用 `$pids`，但当前上下文中没有看到对应变量来源。

建议处理：

- 保留本分支 `$processIds` 写法。
- 同时检查 upstream 是否在其他位置新增安装安全性、TLS、下载、PATH 或自更新逻辑，有独立价值的要合并。

为什么这么处理：

这里本分支写法更自洽，避免引用不存在或含义不清的变量。安装脚本属于用户入口，宁可保守保留已验证的进程清理逻辑。

### 9. Config / real git 探测

冲突文件：

- `src/config.rs`

本分支做过的关键修复：

- 在 PATH 中查找真实 Git 时跳过 git-ai shim。
- Windows installer 复制 `git-ai.exe` 到 `git.exe` 时，使用文件内容相同判断 shim，避免 `same_file` 在复制文件场景失效。
- `prompt_storage` 默认改为 `notes`。

upstream/main 的相关变化：

- Windows 静态候选路径加了 `#[cfg(windows)]`。
- 增加 `LOCALAPPDATA\Programs\Git` 候选。
- 增加 `where.exe git.exe` fallback。
- Windows 下如果 `git.exe` 旁边存在 `git-ai.exe`，直接视为 shim。

建议处理：

- 合并 upstream 的候选路径和 `where.exe` fallback。
- 保留本分支 `files_have_same_contents` 的精确 shim 判断，或至少在注释中明确为什么 Windows sibling 判定足够安全。
- `prompt_storage` 默认值按当前产品决策处理：看板仍要 promptText 快速落库则保留 `notes`。

为什么这么处理：

upstream 的探测范围更完整，本分支的 shim 判断更精确。最佳结果是两者合并，而不是互相覆盖。

### 10. Feature flags

冲突文件：

- `src/feature_flags.rs`
- `tests/integration/performance.rs`

本分支新增：

- `format_only_attribution_passthrough`
- `auto_upload_ai_stats`

upstream/main 新增：

- `transcript_streaming`
- `transcript_sweep`

建议处理：

- 四个 flag 都保留。
- 默认值建议：
  - `format_only_attribution_passthrough`: `debug=true, release=true`
  - `auto_upload_ai_stats`: `debug=true, release=true`
  - `transcript_streaming`: 按 upstream 保留
  - `transcript_sweep`: 按 upstream 保留，注意 release 默认值
- 更新所有 `FeatureFlags { ... }` 测试构造体。

为什么这么处理：

这些 flag 控制的是不同能力，没有互斥关系。简单选一边会导致上传或 transcript 新架构任一方丢失。

### 11. refs / authorship note 兼容测试

冲突文件：

- `src/git/refs.rs`

本分支新增：

- `get_reference_as_working_log` 测试。
- `get_reference_as_authorship_log_v3` schema version mismatch / legacy version 测试。

upstream/main：

- 没有等价测试冲突内容。

建议处理：

- 保留本分支测试。
- 如果 upstream 的 test helper 从 `TmpRepo` 迁移到了新路径，按新 helper 改 import。

为什么这么处理：

这些测试保护 authorship note 兼容性，和 upstream 主逻辑不冲突。

### 12. Cargo / Nix 版本与依赖

冲突文件：

- `Cargo.toml`
- `Cargo.lock`
- `flake.nix`

冲突点：

- `Cargo.toml`：本分支版本 `2.1.1`，upstream 版本 `1.4.5`。
- `flake.nix`：本分支仍写 `2.0.8`，upstream 写 `1.4.5`。
- `Cargo.lock`：依赖树冲突。

建议处理：

- 如果这是团队 fork 发布分支，`Cargo.toml` 保留 `2.1.1`。
- `flake.nix` 不建议保留当前 `2.0.8`，应改成与 `Cargo.toml` 一致的 `2.1.1`。
- `Cargo.lock` 不要手工逐行选边。代码冲突处理完后重新生成并验证。

为什么这么处理：

upstream 版本号代表官方项目版本；你的 fork 已经有自定义发布线。合并 upstream 不等于把 fork 版本降回官方版本。

### 13. 快照文件

冲突文件：

- `src/authorship/snapshots/git_ai__authorship__authorship_log_serialization__tests__file_names_with_spaces.snap`
- `src/authorship/snapshots/git_ai__authorship__authorship_log_serialization__tests__hash_always_maps_to_prompt.snap`
- `src/authorship/snapshots/git_ai__authorship__authorship_log_serialization__tests__serialize_deserialize_no_attestations.snap`

建议处理：

- 不直接选 ours。
- 不直接选 upstream。
- 先解决代码冲突，再运行相关测试和 `cargo insta review`。
- 确认输出符合最终 schema 后再接受新快照。

为什么这么处理：

upstream 引入 sessions，本分支引入 prompt-only / 上传口径，快照最终形态取决于代码合并结果。提前选边容易把 schema 或输出口径固定错。

### 14. integration performance test

冲突文件：

- `tests/integration/performance.rs`

冲突点：

- `FeatureFlags` 结构体初始化字段不一致。

建议处理：

- 合并两边所有字段。
- 测试里显式设置 `auto_upload_ai_stats=false`，避免性能测试被上传链路干扰。
- 保留 upstream 的 `transcript_streaming` / `transcript_sweep` 字段设置。

为什么这么处理：

性能测试应该稳定、离线、可重复。上传功能必须关闭，但 upstream transcript flags 也要构造完整，否则编译不过或测试语义不完整。

## 官方已覆盖 vs 本分支必须移植

| 问题 / 功能 | upstream/main 是否已有有效方案 | 建议 |
|-------------|-------------------------------|------|
| KnownHuman 抢先保存导致 AI 归因丢失 | 有，daemon-side pending AI edit suppression | 用官方方案为主，移植本地诊断和测试 |
| GitHub Copilot `__vscode-*` tool_use_id 后缀 | 未发现等价实现 | 必须移植到 split preset |
| `WillEdit` clean 文本文件基线保留 | 部分相关，需核查新 `path_role` 流程 | 若无等价行为，移植 |
| 原生自动上传 / 手动 `upload-stats` | 没有 | 必须保留 |
| 上传 HTTP 200 body `code/msg` 校验 | 没有 | 必须保留 |
| prompt 仅保留用户输入 | 没有完整等价实现 | 必须保留 |
| `prompt_storage` 默认 `notes` | upstream 默认仍是 `default` | 按团队看板需求保留或单独决策 |
| 空白新增行邻接归因 | 未发现等价实现 | 必须移植 |
| 中文路径逐文件统计 `core.quotepath=false` | 上传模块为本分支独有 | 必须保留在上传模块 |
| daemon 卡死 recovery / replacement runtime | upstream 有部分 daemon 改进，但不是同一问题 | 手工合并两边 |
| test-support 禁止 wrapper 自动拉 daemon | upstream 有 | 必须保留 upstream |
| `bg tail` 默认退出、`-f/--follow` 才跟随 | upstream 有 | 建议保留 upstream |

## 推荐合并顺序

1. 先接受 upstream 的结构性删除和新增：`src/daemon/checkpoint.rs`、`src/commands/checkpoint_agent/presets/*`。
2. 合并 `feature_flags.rs`，保证所有后续代码能引用完整 flags。
3. 合并 `config.rs` 和 `install_hooks.rs`，先稳定运行时配置和 trace2 行为。
4. 合并 checkpoint 行为：以 `src/daemon/checkpoint.rs` 为目标，移植本地缺失诊断/边界行为。
5. 合并 Copilot preset：以 `presets/github_copilot/*` 为目标，移植 `__vscode-*` 后缀兼容。
6. 合并 post_commit / secrets / stats / upload_stats，保证 sessions + 上传 + prompt-only + 文件级统计共存。
7. 合并 daemon 控制和 installer。
8. 最后处理 `Cargo.lock`、snapshots 和测试构造体。

## 合并后的最低验证清单

建议至少跑这些验证：

```powershell
cargo test test_ai_checkpoint_reclaims_recent_known_human_file_capture --lib
cargo test retain_user_prompt_messages_drops_non_user_entries --lib
cargo test test_stats_attributes_trailing_blank_line_to_neighboring_known_human_block --lib
cargo test build_prompt_stats_includes_prompt_text_and_messages --lib
cargo test --test integration github_copilot
cargo test --test integration daemon_mode
git-ai upload-stats --dry-run
```

如果上游新架构已经替代旧测试名，应保留测试意图而不是死守旧测试函数名。

## 当前不建议做的事

- 不建议在 `feature_20260429_gaoang` 主工作区直接继续 `merge upstream/main`。
- 不建议直接 `git checkout --theirs .`，会丢掉本分支上传和多项真实 bug fix。
- 不建议直接 `git checkout --ours .`，会丢掉 upstream checkpoint rewrite / sessions / transcript worker 等新架构。
- 不建议手工解 `Cargo.lock` 和 `.snap` 文件，应该在代码稳定后重新生成。
