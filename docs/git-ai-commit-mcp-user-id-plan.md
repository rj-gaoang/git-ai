# git-ai 在 commit 时读取 IDE MCP 配置并提取 X-USER-ID 的方案

## 0. 已知 MCP 配置方式

### 0.1 X-USER-ID 的来源

代码评审 MCP 服务会用 `X-USER-ID` 识别当前上传人。这个值不是 Git 用户名，也不是本地 IDE 账号名，而是需要先登录 `https://aicr.ruijie.com.cn/`，进入“代码评审记录”里的“【MCP服务方式导入】”页面后复制得到的用户 ID。

也就是说，本文讨论的“获取 MCP 中配置的 `X-USER-ID`”，前提是开发者已经先从平台拿到了自己的真实用户 ID，并把它写进了 MCP 配置。

### 0.2 IDEA 配置特征

IDEA 中通常通过 GitHub Copilot 的工具配置打开 `mcp.json`，常见结构如下：

```json
{
    "servers": {
        "codereview-mcp": {
            "url": "http://mcppage.ruijie.com.cn:9810/mcp",
            "requestInit": {
                "headers": {
                    "X-USER-ID": "XXXXX"
                }
            }
        }
    }
}
```

这里的关键信息是：

- IDEA 示例里，`X-USER-ID` 常见位于 `servers.*.requestInit.headers.X-USER-ID`
- server 名常见为 `codereview-mcp`，但不能假定这个名字永远固定

### 0.3 VS Code 配置特征

VS Code 当前至少有两类配置方式：

1. 工作区级别：当前仓库下的 `.vscode/mcp.json`
2. 全局级别：`%APPDATA%\Code\User\mcp.json`
3. Insiders 兼容：`%APPDATA%\Code - Insiders\User\mcp.json`

工作区或向导生成的配置通常类似：

```json
{
    "servers": {
        "codereview-mcp-server": {
            "url": "http://mcppage.ruijie.com.cn:9810/mcp",
            "type": "http",
            "headers": {
                "X-USER-ID": "105"
            }
        }
    },
    "inputs": []
}
```

也可能生成名称和 URL 都不同的配置，例如：

```json
{
    "servers": {
        "codereview-cc22": {
            "url": "http://localhost:9810/mcp",
            "type": "http",
            "headers": {
                "X-USER-ID": "xxxx"
            }
        }
    },
    "inputs": []
}
```

这里需要明确 3 个事实：

- VS Code 更常见的是 `servers.*.headers.X-USER-ID`，而不是 `requestInit.headers`
- server 名是用户可自定义的，不能只按 `codereview-mcp` 精确匹配
- URL 既可能是直连地址 `http://mcppage.ruijie.com.cn:9810/mcp`，也可能是本地代理地址 `http://localhost:9810/mcp`

因此，`git-ai` 的读取策略必须同时兼容 `requestInit.headers` 和 `headers`，并且把“是否存在 `X-USER-ID`”作为最终判断标准。

## 1. 目标

在 `git-ai` 项目中，希望在开发者执行 `git commit` 后，自动读取本机 VS Code / IDEA 的 `mcp.json` 配置，从中提取 `X-USER-ID`，并把该值用于后续的远程上报请求头。

用户给出的 MCP 配置格式如下：

```json
{
  "servers": {
    "codereview-mcp": {
      "url": "http://mcppage.ruijie.com.cn:9810/mcp",
      "requestInit": {
        "headers": {
          "X-USER-ID": "XXXXX"
        }
      }
    }
  }
}
```

这里的核心诉求不是把 `X-USER-ID` 写进 Git Note，而是让 commit 触发的扩展处理能拿到这个用户标识，并在调用远程接口时带上 `X-USER-ID` 请求头。

## 2. 先说结论

推荐方案不是直接修改 `git-ai` 的 Rust commit 主链路去读取 IDE 配置，而是复用现有的 `git_ai_hooks.post_notes_updated` 扩展点：

1. `git commit` 完成后，`git-ai` 会生成 authorship note。
2. note 写入后，`git-ai` 会调用 `post_notes_updated` 外挂脚本。
3. 这个外挂脚本按优先级读取当前仓库 `.vscode/mcp.json`、VS Code 用户级 `mcp.json` 和 IDEA `mcp.json`。
4. 脚本按“优先 `requestInit.headers.X-USER-ID`，回退 `headers.X-USER-ID`”的顺序提取用户 ID。
5. 脚本把 commit 统计或 note 数据上传到远端时，将该值放入 HTTP 请求头 `X-USER-ID`。

这条路径满足“在 commit 时自动触发”，同时避免把 IDE 私有配置、Windows 路径探测、JSON/JSONC 兼容和 HTTP 上传逻辑硬塞进 `git-ai` 核心。

## 3. 为什么推荐走外部 hook，而不是改 Rust 主链路

### 3.1 当前 `git-ai` 的职责边界已经很清晰

当前仓库的真实职责链路是：

1. agent / IDE hook 产生 checkpoint
2. checkpoint 写入 working log
3. `git commit` 后生成 authorship note
4. note 存到 `refs/notes/ai`
5. 如有配置，再触发 `git_ai_hooks.post_notes_updated`

也就是说，`git-ai` 核心已经提供了“commit 后扩展处理”的标准出口，不需要为了这个需求再侵入 note 生成逻辑。

### 3.2 配置能力也天然更适合外部脚本

当前 `git-ai` 的配置文件支持 `git_ai_hooks.*`，而不适合为“自定义远程上报 + IDE MCP 探测”继续扩张新的顶层配置模型。对于这个需求，最稳的做法是：

- `git-ai` 只负责在正确时机触发扩展命令
- 扩展脚本自己决定怎么找 `mcp.json`
- 扩展脚本自己决定怎么拼远程请求头

### 3.3 IDE MCP 配置本身是强平台相关、强客户端相关的

`mcp.json` 的位置、格式兼容性、是否带注释、是放在当前仓库 `.vscode/mcp.json` 还是 IDE 用户目录、是否放在 `requestInit.headers` 还是 `headers`、以及 server 名称和 URL 是否由向导自动生成，都属于 IDE/客户端侧细节。把这些逻辑放进外部 PowerShell 脚本比放进 Rust 核心更容易迭代，也更不容易影响提交主流程稳定性。

## 4. 推荐的整体架构

### 4.1 时序

推荐时序如下：

```text
IDE/Agent Hook
  -> git-ai checkpoint
  -> working log
  -> git commit
  -> git-ai 生成 authorship note
  -> post_notes_updated 外挂脚本启动
  -> 外挂脚本读取本机 mcp.json
  -> 提取 X-USER-ID
  -> 调用远程上传接口
```

### 4.2 接入点

接入点使用 `git_ai_hooks.post_notes_updated`。

在 `~/.git-ai/config.json` 中增加类似配置：

```json
{
  "git_ai_hooks": {
    "post_notes_updated": [
      "pwsh -NoProfile -File C:/git-ai/scripts/post-notes-upload.ps1"
    ]
  }
}
```

这意味着每次 note 写入后，`git-ai` 都会把 note 相关的 JSON 数据通过 stdin 传给该脚本。

### 4.3 `post_notes_updated` 能拿到什么

`git-ai` 传给外挂脚本的 stdin 是一个 JSON 数组，每个元素至少包含：

```json
[
  {
    "commit_sha": "abcdef1234567890",
    "repo_url": "https://github.com/org/repo.git",
    "repo_name": "repo",
    "branch": "feature/demo",
    "is_default_branch": false,
    "note_content": "{...authorship note json...}"
  }
]
```

这已经足够支撑“按 commit 自动上传”的场景，不需要再去额外查询当前仓库上下文。

## 5. X-USER-ID 读取规则

### 5.1 读取总优先级

结合当前补充的 MCP 配置方式，推荐按下面的顺序取值：

1. `GIT_AI_REPORT_REMOTE_USER_ID`
2. 显式路径覆盖：`GIT_AI_VSCODE_MCP_CONFIG_PATH`、`GIT_AI_IDEA_MCP_CONFIG_PATH`
3. 当前 Git 仓库根目录下的 `.vscode/mcp.json`
4. VS Code 全局 `mcp.json`
5. VS Code Insiders 全局 `mcp.json`
6. IDEA / IntelliJ 默认 `mcp.json`

说明：

- 显式环境变量永远优先，便于 CI、服务账号或临时覆盖。
- 显式路径覆盖优先于默认探测，便于团队后续迁移配置位置。
- 工作区 `.vscode/mcp.json` 应先于 IDE 全局配置，因为这是当前项目最明确、最贴近仓库的配置。
- 文件找到之后，不应只查一种字段形态，而要同时兼容 `requestInit.headers.X-USER-ID` 和 `headers.X-USER-ID`。

### 5.2 默认探测路径

Windows 下建议默认探测以下路径。

当前仓库工作区：

- `<git-repo-root>\.vscode\mcp.json`

VS Code：

- `%APPDATA%\Code\User\mcp.json`
- `%APPDATA%\Code - Insiders\User\mcp.json`

IDEA / IntelliJ：

- `%LOCALAPPDATA%\github-copilot\intellij\mcp.json`
- `%APPDATA%\github-copilot\intellij\mcp.json`

允许通过环境变量覆盖：

- `GIT_AI_VSCODE_MCP_CONFIG_PATH`
- `GIT_AI_IDEA_MCP_CONFIG_PATH`

这里建议脚本在运行时先执行 `git rev-parse --show-toplevel` 来定位当前仓库根目录，再拼接 `.vscode\mcp.json`。这样即使用户是在子目录执行 `git commit`，也能正确命中工作区级 MCP 配置。

### 5.3 服务节点选择规则

基于当前补充的配置方式，推荐先做“排序”，再做“提取”：

1. 先收集 `servers` 下所有 server
2. 优先提升以下 server 的排序：
    - 名称是 `codereview-mcp` 或 `codereview-mcp-server`
    - `url` 包含 `mcppage.ruijie.com.cn:9810/mcp`
    - `url` 包含 `localhost:9810/mcp`
3. 对排序后的每个 server，依次尝试：
    - `requestInit.headers.X-USER-ID`
    - `headers.X-USER-ID`
4. 返回第一个非空 `X-USER-ID`

这样做的原因是：

- IDEA 示例更偏向 `codereview-mcp + requestInit.headers`
- VS Code 示例更偏向 `codereview-mcp-server + headers`
- VS Code 向导生成的 server 名可以是任意值，比如 `codereview-cc22`
- URL 既可能是远端地址，也可能是本地代理地址，因此 URL 更适合做“加权线索”，而不是做唯一匹配条件

## 6. 外挂脚本应如何实现

推荐把逻辑放到 PowerShell 脚本里，例如：

- `scripts/post-notes-upload.ps1`

脚本内部职责拆成 5 步：

1. 从 stdin 读取 `post_notes_updated` 传入的 note 数组
2. 定位当前仓库根目录，并生成 MCP 候选文件列表
3. 兼容 VS Code / IDEA 两类字段风格，解析出 `X-USER-ID`
4. 组装上传请求
5. 调用远程接口并打印 warning，不阻塞 commit

### 6.1 建议的 PowerShell 伪代码

```powershell
function Get-RepoRoot {
    $repoRoot = git rev-parse --show-toplevel 2>$null
    if ($LASTEXITCODE -eq 0 -and $repoRoot) {
        return $repoRoot.Trim()
    }

    return $null
}

function Get-McpCandidatePaths {
    $paths = @()
    $repoRoot = Get-RepoRoot

    if ($env:GIT_AI_VSCODE_MCP_CONFIG_PATH) {
        $paths += $env:GIT_AI_VSCODE_MCP_CONFIG_PATH
    }

    if ($env:GIT_AI_IDEA_MCP_CONFIG_PATH) {
        $paths += $env:GIT_AI_IDEA_MCP_CONFIG_PATH
    }

    if ($repoRoot) {
        $paths += (Join-Path $repoRoot '.vscode\mcp.json')
    }

    if ($env:APPDATA) {
        $paths += (Join-Path $env:APPDATA 'Code\User\mcp.json')
        $paths += (Join-Path $env:APPDATA 'Code - Insiders\User\mcp.json')
        $paths += (Join-Path $env:APPDATA 'github-copilot\intellij\mcp.json')
    }

    if ($env:LOCALAPPDATA) {
        $paths += (Join-Path $env:LOCALAPPDATA 'github-copilot\intellij\mcp.json')
    }

    return $paths | Where-Object { $_ } | Select-Object -Unique
}

function Get-XUserIdFromServer {
    param([hashtable]$Server)

    if (-not $Server) {
        return $null
    }

    $requestInit = $Server['requestInit']
    if ($requestInit -and $requestInit['headers'] -and $requestInit['headers']['X-USER-ID']) {
        return [string]$requestInit['headers']['X-USER-ID']
    }

    if ($Server['headers'] -and $Server['headers']['X-USER-ID']) {
        return [string]$Server['headers']['X-USER-ID']
    }

    return $null
}

function Get-ServerRank {
    param(
        [string]$Name,
        [hashtable]$Server
    )

    $score = 0

    if (@('codereview-mcp', 'codereview-mcp-server') -contains $Name) {
        $score += 100
    }

    $url = [string]$Server['url']
    if ($url -like '*mcppage.ruijie.com.cn:9810/mcp*' -or $url -like '*localhost:9810/mcp*') {
        $score += 50
    }

    if (Get-XUserIdFromServer -Server $Server) {
        $score += 10
    }

    return $score
}

function Get-XUserIdFromMcpFile {
    param([string]$Path)

    if (-not (Test-Path $Path)) {
        return $null
    }

    try {
        $raw = Get-Content -Raw -Path $Path -ErrorAction Stop
        $json = $raw | ConvertFrom-Json -AsHashtable
    } catch {
        Write-Warning "Failed to parse MCP config: $Path"
        return $null
    }

    $servers = $json['servers']

    if (-not $servers) {
        return $null
    }

    $candidates = foreach ($name in $servers.Keys) {
        $server = $servers[$name]
        [pscustomobject]@{
            Name = [string]$name
            Server = $server
            Rank = Get-ServerRank -Name $name -Server $server
        }
    }

    foreach ($candidate in ($candidates | Sort-Object Rank -Descending)) {
        $userId = Get-XUserIdFromServer -Server $candidate.Server
        if ($userId) {
            return $userId
        }
    }

    return $null
}

function Resolve-XUserId {
    if ($env:GIT_AI_REPORT_REMOTE_USER_ID) {
        return [string]$env:GIT_AI_REPORT_REMOTE_USER_ID
    }

    foreach ($path in Get-McpCandidatePaths) {
        $userId = Get-XUserIdFromMcpFile -Path $path
        if ($userId) {
            return $userId
        }
    }

    return $null
}
```

如果后续确认某些 MCP 文件会使用 JSONC 注释，再在 `ConvertFrom-Json` 之前增加一层注释清洗即可。按目前补充到文档里的 IDEA / VS Code 示例，先按标准 JSON 处理就足够。

### 6.2 HTTP 请求头组装规则

上传时建议这样组装：

```powershell
$headers = @{ 'Content-Type' = 'application/json' }

if ($apiKey) {
    $headers['Authorization'] = "Bearer $apiKey"
}

$userId = Resolve-XUserId
if ($userId) {
    $headers['X-USER-ID'] = $userId
}
```

如果读不到 `X-USER-ID`：

- 只打印 warning
- 不让 commit 失败
- 是否允许无 `X-USER-ID` 上传，由服务端要求决定

## 7. 为什么不建议直接把读取逻辑写进 Rust `post-commit`

如果把逻辑直接写进 Rust 核心，通常需要做下面这些额外事情：

1. 在 Rust 里实现 Windows 路径探测
2. 兼容 JSON 和可能的 JSONC 变体
3. 把 `X-USER-ID` 传入 note 写入阶段，或再扩展 hook payload 结构
4. 给 `Config` 增加更多和远程上传相关的字段
5. 为不同 IDE 的配置路径和格式写测试

这些工作并不是不能做，但从当前 `git-ai` 架构看，不值得先走这条重路径。因为真正需要 `X-USER-ID` 的地方是“远程 HTTP 请求”，不是“Git Note 落盘”。

## 8. 如果业务强制要求“必须在 Rust 核心里读”

如果后续被明确要求“不能依赖外部脚本，必须由 `git-ai` 二进制自己在 commit 时读 `mcp.json`”，可以走第二方案。

### 8.1 第二方案的改造点

建议新增一个独立模块，例如：

- `src/integration/ide_mcp.rs`

模块职责：

- 枚举 VS Code / IDEA 默认配置路径
- 读取 `mcp.json`
- 解析 `servers.*.requestInit.headers.X-USER-ID`
- 返回 `Option<String>`

然后在 note 写入后、`post_notes_updated` 调用前，把 `x_user_id` 一并塞进 hook payload，例如：

```json
{
  "commit_sha": "...",
  "repo_name": "...",
  "note_content": "...",
  "x_user_id": "XXXXX"
}
```

### 8.2 第二方案的问题

第二方案虽然更“集中”，但问题也很明确：

- `git-ai` 会开始依赖具体 IDE 的本地文件布局
- commit 路径会承担更多 IO 和解析风险
- 后续 MCP 配置一旦调整，Rust 主程序也要跟着发布

所以它更适合“团队明确要把这件事产品化成 git-ai 原生能力”之后再做，而不是当前第一步落地方案。

## 9. 建议的最终落地方案

### 9.1 方案选择

建议采用：

- `git-ai` Rust 核心不改 commit 主链路
- 使用 `git_ai_hooks.post_notes_updated`
- 在外部 PowerShell 脚本中读取 MCP 配置
- 按“优先 `requestInit.headers.X-USER-ID`，回退 `headers.X-USER-ID`”提取用户 ID
- 上传时附带 `X-USER-ID` 请求头

### 9.2 这套方案的优点

- 真正满足“commit 时自动触发”
- 不阻塞 `git-ai` 核心提交流程
- 与当前仓库已存在的 `post_notes_updated` 架构完全一致
- 支持 VS Code 工作区 `.vscode/mcp.json` 优先于全局配置
- 兼容 IDEA `requestInit.headers` 与 VS Code `headers` 两种主流写法
- 兼容 MCP 向导生成的自定义 server 名称和 `localhost` 代理 URL
- IDE 路径和格式变化时，只需要改脚本，不需要改 Rust 核心
- 可以很自然地和已有的 `upload-ai-stats.ps1` 思路统一

## 10. 验证清单

落地后至少验证以下场景：

1. `GIT_AI_REPORT_REMOTE_USER_ID` 已设置时，优先使用环境变量，不读 `mcp.json`
2. 当前仓库 `.vscode/mcp.json` 存在且包含 `headers.X-USER-ID` 时，能正确取到值
3. 当前仓库 `.vscode/mcp.json` 和 VS Code 全局 `mcp.json` 同时存在时，优先命中工作区配置
4. VS Code 全局 `mcp.json` 存在且包含 `headers.X-USER-ID` 时，能正确取到值
5. IDEA `mcp.json` 存在且包含 `requestInit.headers.X-USER-ID` 时，能正确取到值
6. server 名不是 `codereview-mcp`，而是 `codereview-mcp-server` 或 `codereview-cc22` 时，仍能命中
7. MCP URL 是 `http://localhost:9810/mcp` 时，仍能作为候选命中
8. `mcp.json` 不存在或格式错误时，只打印 warning，不导致 commit 失败
9. `post_notes_updated` 脚本超时或上传失败时，不影响 authorship note 已经成功写入

## 11. 一句话结论

对 `git-ai` 来说，这个需求最合理的实现方式是：

**把“读取 VS Code / IDEA 的 `mcp.json` 并提取 `X-USER-ID`”放到 `post_notes_updated` 外挂脚本里做，而不是直接塞进 Rust 的 commit 主链路。**

这样既满足“commit 时自动完成”，也能保持 `git-ai` 核心职责清晰、风险最小、落地最快。