# git-ai 在保存 commit 信息时读取 MCP user_id 的 Rust 原生改造方案

## 1. 文档目的

本文不是替代现有的外挂脚本方案，而是把“在 git-ai Rust 源码中原生读取 MCP 配置并提取 `X-USER-ID`”整理成一份可执行的设计文档，供后续源码改造使用。

本文主要解决以下问题：

1. 在 `git commit` 完成并写入 authorship note 时，如何从 IDE MCP 配置中读取用户配置的 `X-USER-ID`
2. 如果没有配置该字段，如何保证 `git-ai` 继续正常工作，并把该值视为可空
3. 在不修改 Git Note 核心 schema 的前提下，如何把这个值传给后续扩展处理逻辑

相关背景文档：

- `docs/git-ai-commit-mcp-user-id-plan.md`：已有的外挂脚本优先方案
- `docs/copilot-detection-principle-detailed.md`：当前源码级调用链和关键模块说明

## 2. 改造目标与边界

### 2.1 目标

目标是在 `git-ai` 保存 commit 对应 authorship note 后，为 `post_notes_updated` hook payload 增加一个新的可选字段：

```json
{
  "x_user_id": "105"
}
```

如果未读取到任何有效的 `X-USER-ID`，则该字段应为 `null`，而不是报错或中断 commit。

### 2.2 非目标

本次方案不包含以下事项：

1. 不把 `X-USER-ID` 写入 `refs/notes/ai` 对应的 note 内容
2. 不扩展 `Config` 为新的远程上报配置模型
3. 不在 commit 主链路里直接发 HTTP 请求
4. 不改变当前 `authorship/3.0.0` note schema

## 3. 当前源码调用链与改造落点

根据当前源码，commit 后写 note 并触发 hook 的路径如下：

1. `src/authorship/post_commit.rs`
2. `post_commit_with_final_state()` 序列化 authorship log
3. 调用 `notes_add(repo, &commit_sha, &authorship_json)?;`
4. `src/git/refs.rs` 中的 note 写入逻辑完成 `refs/notes/ai` 更新
5. `src/authorship/git_ai_hooks.rs` 中的 `post_notes_updated()` 组装 JSON payload 并触发外挂命令

当前关键代码点如下：

### 3.1 note 写入发生位置

`src/authorship/post_commit.rs`：

```rust
let authorship_json = authorship_log.serialize_to_string()?;
notes_add(repo, &commit_sha, &authorship_json)?;
```

### 3.2 hook 触发发生位置

`src/git/refs.rs`：

```rust
exec_git_stdin(&fast_import_args, &script)?;
crate::authorship::git_ai_hooks::post_notes_updated(repo, &deduped_entries);
```

### 3.3 payload 组装发生位置

`src/authorship/git_ai_hooks.rs`：

```rust
let payload = notes
    .iter()
    .map(|(commit_sha, note_content)| {
        serde_json::json!({
            "commit_sha": commit_sha,
            "repo_url": repo_url.as_str(),
            "repo_name": repo_name.as_str(),
            "branch": branch.as_str(),
            "is_default_branch": is_default_branch,
            "note_content": note_content,
        })
    })
    .collect::<Vec<_>>();
```

### 3.4 推荐改造落点

推荐把 MCP 配置读取逻辑放在 `src/authorship/git_ai_hooks.rs` 的 `post_notes_updated()` 内，在 payload 组装前只读取一次，然后把结果复用到本次批量 payload 的每一项中。

这样做有 4 个直接好处：

1. 不污染 note 内容本身
2. 不改变 `post_commit` 的主要职责
3. 只在真正需要向外扩展传递数据时才读取本地 IDE 配置
4. 与当前 `post_notes_updated` 的扩展定位完全一致

## 4. 总体设计

### 4.1 设计原则

本方案遵循以下原则：

1. `X-USER-ID` 是可选增强信息，不是 commit 成功的前置条件
2. 所有文件读取、JSON 解析、路径探测失败都必须降级为 `None`
3. 读取逻辑应封装为独立模块，不散落在 hook 组装逻辑中
4. 对现有外部 hook 命令保持向后兼容，只新增字段，不删除字段
5. 优先支持 Windows 场景，同时保持代码结构可扩展到 Linux / macOS

### 4.2 数据流

```text
git commit
  -> post_commit_with_final_state()
  -> notes_add()
  -> refs/notes/ai 写入成功
  -> post_notes_updated(repo, notes)
      -> resolve_x_user_id(repo_workdir)
      -> 生成包含 x_user_id 的 JSON payload
      -> 通过 stdin 传给 post_notes_updated hook
```

## 5. 文件级改造方案

### 5.1 新增文件

#### `src/integration/mod.rs`

模块声明文件：

```rust
pub mod ide_mcp;
```

#### `src/integration/ide_mcp.rs`

新增 MCP 读取模块，职责如下：

1. 枚举 MCP 候选路径
2. 读取并解析 `mcp.json`
3. 兼容 IDEA 和 VS Code 两类字段风格
4. 提取并返回 `Option<String>`

### 5.2 修改文件

#### `src/lib.rs`

增加：

```rust
pub mod integration;
```

#### `src/authorship/git_ai_hooks.rs`

增加对 `resolve_x_user_id()` 的调用，并将结果注入 payload。

## 6. 新模块接口设计

### 6.1 对外公开接口

建议暴露一个最小公开接口：

```rust
pub fn resolve_x_user_id(repo_workdir: Option<&std::path::Path>) -> Option<String>
```

说明如下：

1. `repo_workdir` 用于拼接当前仓库的 `.vscode/mcp.json`
2. 返回值为 `Option<String>`
3. 所有异常都由函数内部吞掉并以 `None` 降级

### 6.2 推荐的内部辅助函数

建议拆成以下几个内部函数，便于测试：

```rust
fn candidate_paths(repo_workdir: Option<&Path>) -> Vec<PathBuf>
fn read_x_user_id_from_file(path: &Path) -> Option<String>
fn parse_x_user_id_from_json(value: &serde_json::Value) -> Option<String>
fn scored_servers(servers: &serde_json::Map<String, serde_json::Value>) -> Vec<ServerCandidate>
fn extract_x_user_id_from_server(server: &serde_json::Value) -> Option<String>
fn server_score(name: &str, server: &serde_json::Value) -> i32
```

## 7. 路径探测规则

### 7.1 总优先级

建议按以下顺序读取：

1. `GIT_AI_REPORT_REMOTE_USER_ID`
2. `GIT_AI_VSCODE_MCP_CONFIG_PATH`
3. `GIT_AI_IDEA_MCP_CONFIG_PATH`
4. `<repo_workdir>/.vscode/mcp.json`
5. `%APPDATA%/Code/User/mcp.json`
6. `%APPDATA%/Code - Insiders/User/mcp.json`
7. `%LOCALAPPDATA%/github-copilot/intellij/mcp.json`
8. `%APPDATA%/github-copilot/intellij/mcp.json`

### 7.2 规则说明

1. 如果 `GIT_AI_REPORT_REMOTE_USER_ID` 存在且非空，直接返回，不再读取文件
2. 显式路径覆盖优先于默认探测路径
3. 仓库内 `.vscode/mcp.json` 优先于 IDE 全局目录
4. 同一个路径只处理一次，避免重复探测

### 7.3 跨平台考虑

当前需求重点是 Windows，但 Rust 原生实现不应把路径逻辑完全写死为 Windows 常量。

建议策略：

1. Windows 路径优先按 `APPDATA` 和 `LOCALAPPDATA` 拼接
2. Linux / macOS 可在后续版本追加 `~/.config/Code/User/mcp.json` 和对应的 IntelliJ 路径
3. 路径拼接统一用 `PathBuf`，不要手写分隔符

## 8. MCP JSON 解析规则

### 8.1 支持的结构

需要同时支持以下两类结构：

#### IDEA 风格

```json
{
  "servers": {
    "codereview-mcp": {
      "url": "http://mcppage.ruijie.com.cn:9810/mcp",
      "requestInit": {
        "headers": {
          "X-USER-ID": "105"
        }
      }
    }
  }
}
```

#### VS Code 风格

```json
{
  "servers": {
    "codereview-mcp-server": {
      "url": "http://localhost:9810/mcp",
      "headers": {
        "X-USER-ID": "105"
      }
    }
  }
}
```

### 8.2 字段读取顺序

对于每个 server，建议按以下顺序读取：

1. `requestInit.headers.X-USER-ID`
2. `headers.X-USER-ID`

只要命中第一个非空值即返回。

### 8.3 server 选择策略

由于 server 名称和 URL 可能因向导或用户自定义而变化，因此不应硬编码精确匹配。建议采用“评分后排序”的方式。

推荐评分规则：

1. server 名为 `codereview-mcp` 或 `codereview-mcp-server`，加 100 分
2. URL 包含 `mcppage.ruijie.com.cn:9810/mcp`，加 50 分
3. URL 包含 `localhost:9810/mcp`，加 25 分
4. 当前 server 中存在非空 `X-USER-ID`，加 10 分

之后按分数从高到低遍历，返回第一个非空的 `X-USER-ID`。

### 8.4 值类型兼容

建议兼容以下两种情况：

1. `"X-USER-ID": "105"`
2. `"X-USER-ID": 105`

实现时优先按字符串读取，若不是字符串但为数值，则转成字符串返回。

### 8.5 JSONC 策略

首版可以只支持标准 JSON。

理由如下：

1. 当前已知示例均为标准 JSON
2. Rust 主链路里不宜先引入额外 JSONC 解析复杂度
3. 如后续确认 IDE MCP 文件允许注释，再增加轻量预处理函数即可

## 9. Hook payload 改造方案

### 9.1 当前 payload

当前 `post_notes_updated` payload 的单项结构为：

```json
{
  "commit_sha": "abcdef1234567890",
  "repo_url": "https://github.com/org/repo.git",
  "repo_name": "repo",
  "branch": "feature/demo",
  "is_default_branch": false,
  "note_content": "{...authorship note json...}"
}
```

### 9.2 改造后的 payload

改造后建议新增字段：

```json
{
  "commit_sha": "abcdef1234567890",
  "repo_url": "https://github.com/org/repo.git",
  "repo_name": "repo",
  "branch": "feature/demo",
  "is_default_branch": false,
  "note_content": "{...authorship note json...}",
  "x_user_id": "105"
}
```

未命中时：

```json
{
  "x_user_id": null
}
```

### 9.3 注入方式

推荐在 `post_notes_updated()` 中只读取一次：

```rust
let x_user_id = crate::integration::ide_mcp::resolve_x_user_id(Some(repo.canonical_workdir()));
```

然后在 `serde_json::json!` 中加入：

```rust
"x_user_id": x_user_id,
```

### 9.4 为什么不写进 Git Note

不建议把 `X-USER-ID` 直接写入 note 内容，原因如下：

1. `X-USER-ID` 是“当前机器上报身份”，不是代码归属元数据
2. authorship note 应保持与身份上报逻辑解耦
3. 修改 note schema 会带来版本兼容和历史数据处理成本

## 10. 错误处理与日志策略

### 10.1 失败处理原则

所有 MCP 读取失败都必须满足以下要求：

1. 不影响 note 写入成功
2. 不影响 commit 成功
3. 不影响后续 hook 命令启动
4. 返回 `x_user_id = null`

### 10.2 建议日志策略

建议只在 debug 级别记录以下异常：

1. 读取文件失败但不是文件不存在
2. JSON 解析失败
3. 文件存在但结构不符合预期

对于“文件不存在”这种正常未命中场景，不建议输出日志，避免污染终端。

## 11. 测试与验证方案

### 11.1 单元测试建议

建议为 `src/integration/ide_mcp.rs` 编写单元测试，覆盖以下场景：

1. `GIT_AI_REPORT_REMOTE_USER_ID` 存在时直接返回
2. `requestInit.headers.X-USER-ID` 能被正确解析
3. `headers.X-USER-ID` 能被正确解析
4. server 名不是固定值，但 URL 命中时仍能返回
5. `X-USER-ID` 为数字时能正确转成字符串
6. JSON 非法时返回 `None`
7. 文件不存在时返回 `None`
8. 多个候选路径同时存在时按优先级命中

### 11.2 集成验证建议

在完成代码改造后，至少做以下手工验证：

1. 当前仓库 `.vscode/mcp.json` 存在时，`post_notes_updated` 收到非空 `x_user_id`
2. 仅存在 VS Code 全局配置时，仍能收到非空 `x_user_id`
3. 仅存在 IDEA 配置时，仍能收到非空 `x_user_id`
4. 完全没有 MCP 配置时，hook payload 中 `x_user_id` 为 `null`
5. MCP 文件格式错误时，commit 不失败，hook 仍被触发

## 12. 兼容性与风险

### 12.1 兼容性

该方案对现有行为的影响如下：

1. 对 note 内容无影响
2. 对已有 hook 命令无破坏性影响
3. 对未使用 `post_notes_updated` 的用户无行为变化
4. 对旧版脚本完全兼容，因为只是新增字段

### 12.2 风险点

主要风险如下：

1. IDE 后续可能调整 `mcp.json` 路径或字段结构
2. 某些环境可能使用 JSONC，需要后续补充兼容
3. 如果未来出现多个 MCP server 同时携带 `X-USER-ID`，评分规则需要持续维护

## 13. 实施顺序建议

建议按以下顺序实施：

1. 新增 `src/integration/mod.rs`
2. 新增 `src/integration/ide_mcp.rs`
3. 在 `src/lib.rs` 暴露 `integration` 模块
4. 在 `src/authorship/git_ai_hooks.rs` 中接入 `resolve_x_user_id()`
5. 为新模块补充单元测试
6. 用现有 `post_notes_updated` hook 做端到端验证

## 14. 一句话结论

如果后续决定把“读取 MCP 中配置的用户 ID”做成 `git-ai` 的原生能力，最稳妥的落点不是修改 Git Note 内容，而是：

**在 `post_notes_updated()` 组装 hook payload 时原生读取 MCP 配置，将 `X-USER-ID` 解析为 `Option<String>`，并以新增字段 `x_user_id` 传给后续扩展处理逻辑。**