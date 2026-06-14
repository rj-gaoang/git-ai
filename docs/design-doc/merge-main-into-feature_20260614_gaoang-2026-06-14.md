# 2026-06-14 main 合并到 feature_20260614_gaoang 记录

## 1. 合并目标与基线

本次操作目标是把官方镜像 `main` 合并到个人开发分支，且遵守两条约束：

1. 如果官方代码和个人分支代码在解决同一个问题，以官方实现为主。
2. 不能因为合并引入新的编译错误、模块歧义或运行路径错误。

实际分支状态：

| 项目 | 值 |
|------|----|
| 原开发分支 | `feature_20260526_gaoang` |
| 新建合并分支 | `feature_20260614_gaoang` |
| 合入来源 | `main` |
| 合入来源提交 | `eeed6f609` |
| 新分支创建点 | `aeb0911cc` |

已执行的关键命令：

```powershell
git merge --abort
git switch -c feature_20260614_gaoang
git merge main
```

说明：`git merge --abort` 用于停止原分支上的未完成合并状态；后续所有冲突解决都发生在 `feature_20260614_gaoang`。

## 2. 冲突文件与解决原则

| 文件 | 代码段 | 判断 | 解决方式 |
|------|--------|------|----------|
| `install.ps1` | `Set-PathEnsureContains`、`git.exe` shim 刷新、上传活动锁 | 官方和个人分支都在解决 Windows 安装稳定性，属于同类问题 | 采用官方无管理员 User PATH 和只刷新已有 `git.exe` shim 的方式；保留个人分支的 `GIT_AI_GITHUB_REPO`、`GIT_AI_BINARY_BASE_URL`、`RUIJIE_AI_GIT_AI_BASE_URL`、`Acquire-UploadActivityLock` |
| `src/authorship/post_commit.rs` | prompt 存储、note 写入、stats 计算、自动上传 | 官方解决 post-commit stats 性能和 metrics hunk 复用；个人分支解决看板上传和 prompt 落库 | stats 主路径采用官方 `stats_for_commit_stats_from_hunks`；保留个人分支的 prompt notes/CAS、debug 事件和 `maybe_upload_after_commit` |
| `src/authorship/stats.rs` | `stats_for_commit_stats`、`stats_for_commit_stats_from_hunks`、逐文件 accepted 归因 | 官方解决重复 diff/性能；个人分支解决上传逐文件 payload 和空白新增行归因 | 采用官方 hunk 入口；保留个人分支 `FileAcceptedLineStats`、`accepted_lines_from_attestations_by_file`、空白新增行邻接归并 |
| `src/commands/install_hooks.rs` | `InstallOptions`、Visual Studio opt-in、Trace2 daemon、安装成功上传 | 官方解决 hooks 安装结构和 Visual Studio 扩展 opt-in；个人分支解决现场 hooksPath 失效和安装验证上传 | 采用官方 `InstallOptions` / `--visual-studio-extension`；保留 `repair_stale_global_hooks_path`、Windows 孤儿 daemon 清理、`maybe_upload_install_success` |
| `src/feature_flags.rs` | feature flag 宏定义和测试 | 官方新增 `checkpoint_debug_log` 并将 `transcript_sweep` release 默认打开；个人分支新增看板上传和 hooks flags | 合并两边 flag，`transcript_sweep` release 默认采用官方 `true`，保留 `auto_upload_ai_stats` 默认开启 |

## 3. 精确代码段说明

### 3.1 `install.ps1`

保留个人分支内部分发能力：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `install.ps1:291` | `GIT_AI_GITHUB_REPO` | 允许安装脚本运行时切换到 `rj-gaoang/git-ai` 等 fork 仓库 |
| `install.ps1:370` | `GIT_AI_BINARY_BASE_URL` / `RUIJIE_AI_GIT_AI_BASE_URL` | 保留内部二进制镜像源，官方没有覆盖这个企业分发需求 |
| `install.ps1:701` | `Acquire-UploadActivityLock` | 保留安装/更新时与上传任务互斥，避免 Windows 下替换 binary 时和上传后台任务抢文件 |

采用官方安装稳定性实现：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `install.ps1:413` | `Set-PathEnsureContains` | 采用官方 User PATH 追加方案，不再要求管理员权限修改 Machine PATH，也不再强行插到 Git 之前 |
| `install.ps1:727` | `# Refresh git.exe for existing wrapper users` | 官方只刷新已经存在的 `git.exe` wrapper，避免新安装强制劫持 `git` |
| `install.ps1:851` | `$uploadActivityLock.Dispose()` | 保留个人分支锁释放，确保正常安装结束后释放上传活动锁 |

舍弃个人分支旧实现：

| 旧代码段 | 原因 |
|----------|------|
| `Get-StdGitPath` / `git-og.cmd` 写入 | 官方新安装链路已弱化 wrapper 劫持，不再需要强制检测标准 Git 并创建 `git-og.cmd` |
| `Set-PathPrependBeforeGit` | 和官方“无管理员、User PATH、无定位逻辑”解决同一问题，按原则采用官方 |
| 首次安装时写 `config.json` 的 `async_mode=true` | 当前 Rust 侧 `persist_install_config` 已处理 API 配置和 git_path 回填，避免脚本重复写旧结构 |

### 3.2 `src/authorship/post_commit.rs`

保留个人分支看板链路：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/authorship/post_commit.rs:269` | `retain_user_prompt_messages` | 只保留用户 prompt，避免 assistant/tool 输出进入 note 或上传 |
| `src/authorship/post_commit.rs:271` | `match effective_storage` | 保留 `PromptStorageMode::Notes/Local/Default` 行为，支持 notes 模式稳定落库 prompt |
| `src/authorship/post_commit.rs:323` | `post_commit_authorship_note_write_started` | 保留 debug.jsonl 诊断事件，方便排查 note 是否写入 |
| `src/authorship/post_commit.rs:546` | `post_commit_upload_dispatch_requested` | 保留提交后自动上传前的诊断事件 |
| `src/authorship/post_commit.rs:556` | `maybe_upload_after_commit` | 保留看板自动上传入口，上传失败不影响 commit |

采用官方 post-commit stats 主路径：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/authorship/post_commit.rs:387` | `stats_for_commit_stats_from_hunks` | 采用官方一次 diff hunk 复用，避免旧版 `stats_for_commit_stats` 重复调用 Git |
| `src/authorship/post_commit.rs:434` | `record_commit_metrics` | 保留官方 metrics 参数，包括 `authorship_note_str` 和 `hunks_json` |

关键合并点：

```rust
let computed = stats_for_commit_stats_from_hunks(
    repo,
    &commit_sha,
    &ignore_patterns,
    &diff_hunks,
    Some(&authorship_log),
)?;

crate::integration::upload_stats::maybe_upload_after_commit(
    repo,
    &commit_sha,
    &authorship_log,
    stats.as_ref(),
    will_recompute_missing_stats_for_upload,
    &ignore_patterns,
);
```

这个组合确保官方性能优化和个人分支看板上传同时生效。

### 3.3 `src/authorship/stats.rs`

采用官方统计入口：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/authorship/stats.rs:443` | `stats_for_commit_stats` | 使用官方 `get_diff_with_line_numbers` 获取 hunk |
| `src/authorship/stats.rs:772` | `stats_for_commit_stats_from_hunks` | 保留官方“传入预计算 hunks”接口，供 post-commit 快路径复用 |

保留个人分支逐文件归因能力：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/authorship/stats.rs:54` | `FileAcceptedLineStats` | 上传 payload 需要逐文件 AI/人工 accepted 统计 |
| `src/authorship/stats.rs:482` | `accepted_lines_from_attestations_by_file` | 保留逐文件统计入口，供 `upload_stats.rs` 调用 |
| `src/authorship/stats.rs:596` | `accepted_lines_from_attestations_with_repo` | 聚合逐文件结果为 commit 级 stats |
| `src/authorship/stats.rs:696` | `infer_whitespace_only_added_lines` | 保留空白新增行邻接归并，避免本地 stats 和上传 payload 对空白行口径不一致 |

关键合并点：

```rust
let (ai_accepted, known_human_accepted, ai_accepted_by_tool) =
    accepted_lines_from_attestations_with_repo(
        repo,
        commit_sha,
        authorship_log,
        &added_lines_by_file,
        is_merge_commit,
    );
```

这里是在官方 `stats_for_commit_stats_from_hunks` 内部调用个人分支的逐文件归因函数。原因是二者不冲突：官方解决 diff 获取性能，个人分支解决看板上传统计口径。

### 3.4 `src/commands/install_hooks.rs`

采用官方 hooks 安装结构：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/commands/install_hooks.rs:18` | `VISUAL_STUDIO_INSTALLER_ID` | 保留官方 Visual Studio 扩展 opt-in |
| `src/commands/install_hooks.rs:21` | `InstallOptions` | 保留官方统一参数结构，避免继续使用旧的散落 `dry_run` / `verbose` 局部变量 |
| `src/commands/install_hooks.rs:478` | `parse_install_options` | 保留 `--visual-studio-extension` 参数 |

保留个人分支现场修复：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/commands/install_hooks.rs:17` | `CORE_HOOKS_PATH_KEY` | 支撑失效全局 hooksPath 清理 |
| `src/commands/install_hooks.rs:319` | `repair_stale_global_hooks_path` | 删除不存在的旧 `core.hooksPath`，修复 commit 根本不进 post-commit hook 的问题 |
| `src/commands/install_hooks.rs:390` | `stop_orphaned_managed_daemon_processes` | Windows 下重启 daemon 前清理同 binary 的残留 `git-ai.exe bg run` |
| `src/commands/install_hooks.rs:472` | `maybe_upload_install_success` | 安装成功后发送测试上传，形成安装生效闭环 |

关键合并点：

```rust
let options = parse_install_options(args);

match repair_stale_global_hooks_path(options.dry_run) {
    Ok(Some(path)) if options.dry_run => {
        println!("Would remove stale global core.hooksPath: {}", path);
    }
    Ok(Some(path)) => {
        println!("Removed stale global core.hooksPath: {}", path);
    }
    Ok(None) => {}
    Err(e) => {
        eprintln!("Warning: could not repair core.hooksPath (non-fatal): {e}");
    }
}
ensure_daemon(options.dry_run);
```

这里避免重新引入旧的 `dry_run` 局部变量，全部跟随官方 `InstallOptions`。

### 3.5 `src/feature_flags.rs`

合并后的 flag：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/feature_flags.rs:83` | `async_mode` | 保留个人分支 async 配置 |
| `src/feature_flags.rs:87` | `auto_upload_ai_stats` | 保留看板自动上传开关，默认开启 |
| `src/feature_flags.rs:89` | `transcript_sweep` | 采用官方 release 默认 `true` |
| `src/feature_flags.rs:90` | `checkpoint_debug_log` | 保留官方新增 checkpoint debug flag |
| `src/feature_flags.rs:128` | `if result.git_hooks_enabled { result.async_mode = true; }` | 保留个人分支 hooks 启用时强制 async 的约束 |

测试同步调整：

| 行号 | 代码段 | 说明 |
|------|--------|------|
| `src/feature_flags.rs:140` | `test_default_feature_flags` | 同时断言官方和个人分支新增 flag |
| `src/feature_flags.rs:219` | `test_serialization` | 序列化覆盖 `auto_upload_ai_stats` 和 `checkpoint_debug_log` |
| `src/feature_flags.rs:247` | `test_clone_trait` | clone 覆盖全部合并后的 flag |

### 3.6 非显式冲突但由合并引发的编译修复

| 文件 | 行号 | 问题 | 处理 |
|------|------|------|------|
| `src/authorship/prompt_utils.rs` | `91-102` | 官方把 `transcripts` 模块重命名为 `streams`，个人分支仍引用旧路径 | 改为 `crate::streams::sweep::StreamFormat` 和 `crate::streams::model_extraction::extract_model` |
| `src/git/mod.rs` | `11` | `pub mod test_utils;` 被合并成重复声明 | 保留一处声明 |
| `src/git/test_utils/mod.rs` | `234`、`257`、`362` | 官方新增 `commit_all` / `rebase_onto` API；字段 `transcript_source` 重命名为 `stream_source` | 在目录版测试工具中补充 API，并按官方字段名改为 `stream_source` |
| `src/git/test_utils.rs` | 全文件 | 官方新增文件与个人分支已有目录 `src/git/test_utils/mod.rs` 同名，Rust 编译出现模块歧义 | 删除文件版，保留能力更完整的目录版，并把官方新增 API 合入目录版 |

## 4. 验证结果

已执行：

```powershell
rg -n "^(<<<<<<<|=======|>>>>>>>)" install.ps1 src/authorship/post_commit.rs src/authorship/stats.rs src/commands/install_hooks.rs src/feature_flags.rs
cargo fmt --check
cargo check --features test-support
git diff --check
```

结果：

| 命令 | 结果 |
|------|------|
| 冲突标记扫描 | 通过，目标冲突文件无 `<<<<<<<` / `=======` / `>>>>>>>` |
| `cargo fmt --check` | 通过 |
| `cargo check --features test-support` | 通过 |
| `git diff --check` | 通过，仅有 Windows 换行提示，无 whitespace error |

## 5. 风险与后续建议

1. 本次已通过 Rust 编译检查，但尚未运行完整测试套件。建议后续补跑 `cargo test --features test-support --lib feature_flags`、`cargo test --features test-support --lib integration::upload_stats` 和安装脚本相关测试。
2. `install.ps1` 采用官方“只刷新已有 `git.exe` shim”的策略，新安装不会强行创建 wrapper；这是为了避免和官方安装设计冲突。如果团队仍要求安装即接管 `git` 命令，需要另起明确需求，不建议在合并冲突里隐式恢复。
3. `src/git/test_utils.rs` 被删除不是丢弃官方能力，而是消除 Rust 模块歧义；官方新增的 `commit_all` / `rebase_onto` 已合入 `src/git/test_utils/mod.rs`。
4. 自动上传链路保留在 `post_commit` 末尾，仍是 best-effort，不会让 commit 因远程看板上传失败而失败。
