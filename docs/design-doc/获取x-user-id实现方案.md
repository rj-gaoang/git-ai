# 读取 x_user_id 的实现思路

本文只描述一个通用实现方案，用于从本机环境变量和 IDE MCP 配置中读取用户配置的 `X-USER-ID`，并输出为可直接复用的 `x_user_id`。

这个方案的目标很简单：

1. 独立运行，只依赖本地环境变量和 MCP 配置文件
2. 支持输出纯文本或 JSON，方便脚本和程序调用
3. 支持 trace 模式，便于排查实际命中的配置路径
4. 采用严格 JSON 解析 MCP 文件，不接受 JSONC 注释和尾逗号

适用场景：

1. 其他项目也需要读取相同的用户 ID
2. 希望把 `x_user_id` 读取逻辑单独抽成一个公共脚本
3. 只需要一个可执行脚本，在本地或 CI 中直接拿到值

脚本落地建议：

1. 将脚本放在项目内的公共工具目录，例如 `scripts/get-x-user-id.ps1`
2. 调用方只依赖脚本输出，不依赖脚本所在仓库名称

## 读取优先级

脚本按以下顺序查找 `x_user_id`：

1. 强制覆盖环境变量，例如 `X_USER_ID_OVERRIDE`
2. 显式指定 VS Code MCP 文件的环境变量，例如 `VSCODE_MCP_CONFIG_PATH`
3. 显式指定 IntelliJ MCP 文件的环境变量，例如 `IDEA_MCP_CONFIG_PATH`
4. 当前项目下的 `.vscode/mcp.json`
5. `%APPDATA%\Code\User\mcp.json`
6. `%APPDATA%\Code - Insiders\User\mcp.json`
7. `%APPDATA%\github-copilot\intellij\mcp.json`
8. `%LOCALAPPDATA%\github-copilot\intellij\mcp.json`

说明：

1. 上面的环境变量名只是实现示例，不是固定要求
2. 实际项目可以按团队规范改成自己的变量名

## 支持的 MCP 结构

脚本同时支持两种常见结构：

1. `servers.<name>.requestInit.headers.X-USER-ID`
2. `servers.<name>.headers.X-USER-ID`

示例一：

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

示例二：

```json
{
  "servers": {
    "codereview-mcp-server": {
      "url": "http://localhost:9810/mcp",
      "headers": {
        "X-USER-ID": 108
      }
    }
  }
}
```

## Server 选择规则

如果同一个 MCP 文件里有多个 server，脚本会按评分选出最像代码评审 MCP 的那一个：

1. 名称是 `codereview-mcp` 或 `codereview-mcp-server`，加 100 分
2. URL 包含 `mcppage.ruijie.com.cn:9810/mcp`，加 50 分
3. URL 包含 `localhost:9810/mcp`，加 25 分
4. 当前 server 存在非空 `X-USER-ID`，加 10 分

## 实现规则

建议脚本按以下规则实现：

1. 如果命中强制覆盖环境变量，直接返回，不再继续扫描配置文件
2. 如果需要从文件读取，则按优先级顺序逐个尝试
3. 如果一个 MCP 文件中存在多个 server，则先评分再选中得分最高且包含 `X-USER-ID` 的项
4. 如果没有找到值，纯文本模式不输出内容，JSON 模式输出空结构，并返回非零退出码

## 输出设计

默认输出：

```text
108
```

加 `-AsJson` 时输出：

```json
{
  "XUserId": "108",
  "Source": "repo:.vscode/mcp.json",
  "Path": "D:\\project\\.vscode\\mcp.json",
  "ServerName": "codereview-mcp-server"
}
```

如果没有找到值：

1. 默认模式下不输出内容，退出码为 `1`
2. `-AsJson` 模式下输出 `XUserId = null`，退出码仍为 `1`
3. `-ShowTrace` 模式下会额外打印每个候选路径的尝试过程

## 调用方式示例

### 1. 只输出 x_user_id

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1"
```

### 2. 输出 JSON 和来源

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1" -AsJson
```

### 3. 输出 JSON 并打印查找路径

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1" -AsJson -ShowTrace
```

### 4. 指定目标项目作为 RepoRoot

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1" -RepoRoot "D:\your-project" -AsJson
```

### 5. 用环境变量强制覆盖

```powershell
$env:X_USER_ID_OVERRIDE = "force-20260424"
powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1" -AsJson
```

### 6. 用环境变量显式指定 MCP 文件

```powershell
$env:VSCODE_MCP_CONFIG_PATH = "D:\your-project\.vscode\mcp.json"
powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1" -AsJson
```

## 集成示例

### 在其他 PowerShell 脚本里直接读取

```powershell
$xUserId = powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1"
if ($LASTEXITCODE -ne 0) {
    throw "x_user_id not found"
}

Write-Host "Resolved x_user_id: $xUserId"
```

### 作为 JSON 结果供调用方消费

```powershell
$result = powershell -NoProfile -ExecutionPolicy Bypass -File ".\scripts\get-x-user-id.ps1" -AsJson | ConvertFrom-Json
if (-not $result.XUserId) {
    throw "x_user_id not found"
}

$headers = @{ "X-USER-ID" = $result.XUserId }
```

## 设计边界

这个实现只负责读取并返回 `x_user_id`，不负责：

1. 维护其他系统的附加元数据
2. 发送 HTTP 请求
3. 改写 MCP 文件
4. 自动安装任何 IDE 配置

## 一句话结论

如果一个项目只需要“读取本机配置的 `x_user_id`”，最稳妥的做法就是把这套逻辑做成一个独立脚本：先查环境变量，再查 MCP 配置文件，命中后统一输出为纯文本或 JSON，供其他脚本和程序复用。