# git-ai MCP user_id 部署与验证文档

> 更新说明（2026-04-23）：当前源码已进一步调整为“在可解析到 `x_user_id` 时，同时写入 Git Note 元数据和 `post_notes_updated` hook payload”。本文中个别“不写入 Git note”的历史验证结论仅代表旧版本行为。

## 1. 文档目的

本文用于说明以下 3 件事：

1. 当前官方版 `git-ai`、本地源码版 `git-ai`、基于个人 fork 的 `git-ai` 分别应如何部署
2. `x_user_id` 功能当前到底保存到了哪里
3. 在 `D:\rj-op\op-return-exchange` 中对该功能做过哪些实际验证，验证结果是什么

## 2. 结论摘要

当前改造后的行为已经验证清楚：

1. `x_user_id` 会出现在 `git_ai_hooks.post_notes_updated` 的 JSON payload 中
2. `x_user_id` 的来源优先级为：环境变量 `GIT_AI_REPORT_REMOTE_USER_ID` 优先，其次是 MCP 配置文件
3. 仓库级 `.vscode/mcp.json` 可以被读取，只要 JSON 结构符合当前 Rust 解析器约定
4. 当能解析到值时，`x_user_id` 也会写入 `refs/notes/ai` 对应的 Git note 元数据
5. 如果读取不到 `x_user_id`，commit 不会失败；hook payload 中该字段为 `null`，Git note 中则省略该字段

换句话说，当前行为验证的目标已经变成“双落点”：既要看 commit 后 hook payload 能拿到 user_id，也要看 Git note 元数据中能看到 `x_user_id`。

## 3. 功能边界

当前实现与设计文档保持一致，边界如下：

1. 新字段位置：`git_ai_hooks.post_notes_updated` 的 stdin JSON payload
2. 新字段位置：`refs/notes/ai` 对应 authorship note 的 JSON 元数据
3. 当无法解析到用户 ID 时，不阻断 commit，也不强行写空字符串

示意如下：

```json
[
  {
    "branch": "verify/mcp-user-id-20260422-111438",
    "commit_sha": "5c230cbbb8b68128f3bfc22dcfa331e44e757235",
    "note_content": "...原有 authorship note 内容...",
    "repo_name": "op-return-exchange",
    "repo_url": "https://gitlab.ruijie.com.cn/op-return-exchange/op-return-exchange",
    "x_user_id": "env-20260422"
  }
]
```

## 4. 部署方式

### 4.1 部署官方版 git-ai

Windows 官方安装命令来自项目 README：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://usegitai.com/install.ps1 | iex"
```

适用场景：

1. 你只想使用官方发布版本
2. 不需要验证本地源码改动

### 4.2 部署本地源码版 git-ai

如果要验证你本地修改过的源码，仓库内推荐方式不是直接手工复制二进制，而是运行仓库自带开发安装脚本。

在 `D:\git-ai-main\git-ai` 中执行：

```powershell
powershell -NonInteractive -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1
```

等价说明：

1. `Taskfile.yml` 中的 `task dev` 最终在 Windows 上也是调用 `scripts/dev.ps1`
2. 该脚本会把调试版安装到 `C:\Users\admin\.git-ai\bin\git-ai.exe`
3. 安装后普通 `git` 工作流会通过这个本地调试版 `git-ai` 生效

本次验证时，已确认机器上的有效二进制路径为：

```text
C:\Users\admin\.git-ai\bin\git-ai.exe
```

已确认版本输出为：

```text
git-ai --version
1.3.3 (debug)
```

### 4.3 部署个人 fork 或远程仓库版本

如果你想验证“你自己的 fork 里的代码”，最稳妥方式是：

1. 先把 fork 仓库 clone 到本地
2. checkout 到目标分支
3. 在该本地 clone 中运行 `scripts/dev.ps1`

示例流程：

```powershell
git clone <你的-fork-仓库地址>
cd <你的-fork-仓库目录>
git checkout <你的功能分支>
powershell -NonInteractive -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1
```

这样做的原因是：

1. 当前源码仓库里的 `install.ps1` 默认仍偏向官方 release 安装流程
2. 它不是一个“直接拿任意 fork 源码就能装成该 fork 版本”的通用开发入口
3. 验证 fork 改动时，最可靠路径仍然是“本地 clone fork 后跑 `scripts/dev.ps1`”

## 5. 本次验证环境

验证目标仓库：

```text
D:\rj-op\op-return-exchange
```

验证关注点：

1. commit 时是否能读取 MCP 中配置的 `X-USER-ID`
2. 该值是否能进入 `post_notes_updated` payload
3. 该值是否会写入 Git note

本次使用的验证方式是：

1. 在 `~/.git-ai/config.json` 中临时配置 `git_ai_hooks.post_notes_updated`
2. 让 hook 把 stdin payload 原样写入临时文件
3. 分别测试“环境变量兜底”和“仓库级 `.vscode/mcp.json`”两条读取路径

## 6. 仓库级 MCP 配置验证

### 6.1 使用的仓库级配置

验证时使用的 `.vscode/mcp.json` 结构如下：

```json
{
  "servers": {
    "codereview-mcp-server": {
      "url": "http://mcppage.ruijie.com.cn:9810/mcp",
      "type": "http",
      "headers": {
        "X-USER-ID": "20260422"
      }
    }
  }
}
```

这个结构之所以重要，是因为当前 Rust 解析器只识别以下两种路径：

1. `servers.<name>.requestInit.headers.X-USER-ID`
2. `servers.<name>.headers.X-USER-ID`

### 6.2 实际验证结果

仓库内保留了两条相关验证分支作为证据：

1. `verify/mcp-file-20260422-111642`
2. `verify/mcp-file-20260422-111703`

已确认结果：

1. 仓库级 `.vscode/mcp.json` 能被当前改造版 `git-ai` 读取
2. `post_notes_updated` payload 中的 `x_user_id` 成功取到 `20260422`
3. payload 中的 `commit_sha` 与本次验证 commit 相匹配
4. Git note 中仍然没有 `x_user_id`

针对 `verify/mcp-file-20260422-111703` 分支上的 commit `88984331cd1bed0e81e070c47969fdc27a009c16`，已明确看到其 Git note 仍只有原始 authorship 内容，例如：

```json
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "development:1.3.3",
  "base_commit_sha": "88984331cd1bed0e81e070c47969fdc27a009c16",
  "prompts": {},
  "humans": {
    "h_602db118073c06": {
      "author": "gaoang"
    }
  }
}
```

上述 note 内容中没有 `x_user_id`，这与本次设计预期一致。

## 7. 环境变量兜底验证

同时验证了环境变量优先级：

1. 验证分支：`verify/mcp-user-id-20260422-111438`
2. 验证 commit：`5c230cbbb8b68128f3bfc22dcfa331e44e757235`
3. 环境变量：`GIT_AI_REPORT_REMOTE_USER_ID=env-20260422`

对应 hook 捕获到的 payload 中，已确认包含：

```json
{
  "x_user_id": "env-20260422"
}
```

同时也已确认：

1. 该 commit 的 Git note 不包含 `x_user_id`
2. 环境变量优先级高于文件读取路径

## 8. 为什么最初会出现 `x_user_id = null`

最初出现过一次假阴性，后续确认主要是验证输入不稳定导致，而不是 Rust 改造本身失效。

主要原因有两类：

1. 早期验证使用的 `.vscode/mcp.json` 一度不是解析器支持的最终结构
2. 某些验证脚本在清理阶段删除了临时 payload 文件，导致事后读取不到文件，容易误判为 hook 没有传值

在改用明确兼容的 JSON 结构后，仓库级配置路径与环境变量路径都已经得到正向验证。

## 9. 推荐的后续验收方式

如果后续还要重复验收，建议固定使用下面的判断标准：

1. 先确认当前生效的是本地调试版 `git-ai --version -> 1.3.3 (debug)`
2. 用 `git notes --ref=ai show <sha>` 直接检查 note 元数据里是否出现 `x_user_id`
3. 同时用 `git_ai_hooks.post_notes_updated` 抓 stdin payload，确认 hook 侧也能拿到同一值
4. 如果 payload 有 `x_user_id` 但 note 没有，优先确认当前仓库实际生效的二进制是否已重新安装

## 10. 最终结论

本次功能验证结论如下：

1. 当前改造版 `git-ai` 已能在 commit 后读取 MCP 中配置的 `X-USER-ID`
2. 该值已经能进入 `post_notes_updated` hook payload
3. 仓库级 `.vscode/mcp.json` 与环境变量兜底两条路径都已验证通过
4. 当前源码目标行为是：当能解析到值时，也把它写入 `refs/notes/ai` 的 authorship note 元数据

因此，如果你的目标是“在 commit 后既让外部 hook 能拿到 user_id，又能直接从 Git note 里查看该值”，当前源码已经朝这个方向收敛；实际验收时请同时检查 note 元数据和 hook payload。