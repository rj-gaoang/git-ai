# VS Code 中的 Agent 钩子（预览版）

Hooks run custom shell commands at specific points during an agent session. While instructions and prompts guide what the AI does, hooks guarantee that your code runs at defined lifecycle points. This makes hooks the right choice when you need deterministic outcomes, such as running a formatter after every file edit, blocking commits that fail a lint check, or logging every tool invocation for an audit trail

钩子（Hooks）允许你在 Agent 会话的关键生命周期节点执行自定义 Shell 命令。使用钩子可以自动化工作流、执行安全策略、验证操作，以及与外部工具集成。

关于钩子在 AI 定制化框架中的背景说明，请参阅 [定制化概念](/docs/copilot/concepts/customization.md#hooks)。

本文介绍如何在 VS Code 中配置和使用钩子。

> [!NOTE]
> Agent 钩子目前处于预览阶段，配置格式和行为在后续版本中可能发生变化。

> [!IMPORTANT]
> 你的组织可能已在 VS Code 中禁用了钩子功能。如需了解详情，请联系管理员，或参阅[企业策略](/docs/enterprise/policies.md)。

> [!TIP]
> 使用[聊天定制化编辑器](/docs/copilot/customization/overview.md#chat-customizations-editor)（预览版）可在一处发现、创建和管理所有聊天定制项。从命令面板运行 **Chat: Open Chat Customizations**。

钩子支持跨 Agent 类型工作，包括本地 Agent、后台 Agent 和云端 Agent。每个钩子接收结构化的 JSON 输入，并可返回 JSON 输出以影响 Agent 行为。

## 为什么使用钩子？

钩子提供确定性的、代码驱动的自动化能力。与指导 Agent 行为的指令或自定义提示词不同，钩子在特定生命周期节点以有保证的结果执行你的代码：

* **执行安全策略**：在 `rm -rf` 或 `DROP TABLE` 等危险命令执行之前将其阻断，无论 Agent 如何被提示。

* **自动化代码质量**：在文件修改后自动运行格式化工具、代码检查器或测试。

* **创建审计追踪**：记录每次工具调用、命令执行或文件变更，用于合规和调试。

* **注入上下文**：添加项目特定信息、API 密钥或环境详情，帮助 Agent 做出更好的决策。

* **控制审批**：自动批准安全操作，同时要求对敏感操作进行手动确认。

## 快速入门：你的第一个钩子

以下示例创建一个在每次文件编辑后运行 Prettier 的钩子。在工作区创建 `.github/hooks/format.json` 文件：

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "type": "command",
        "command": "npx prettier --write \"$TOOL_INPUT_FILE_PATH\""
      }
    ]
  }
}
```

保存此文件后，VS Code 会自动加载该钩子。下次 Agent 编辑文件时，Prettier 会对修改后的文件运行。查看 **GitHub Copilot Chat Hooks** 输出通道可验证钩子是否执行。

关于使用自定义脚本的复杂钩子，请参阅[使用场景](#usage-scenarios)。

## 钩子生命周期事件

VS Code 支持八种钩子事件，在 Agent 会话的特定节点触发：

| 钩子事件            | 触发时机                       | 常见用途                                     |
| ------------------ | ------------------------------ | -------------------------------------------- |
| `SessionStart`     | 用户在新会话中提交第一条提示词 | 初始化资源、记录会话启动、验证项目状态        |
| `UserPromptSubmit` | 用户提交提示词                 | 审计用户请求、注入系统上下文                  |
| `PreToolUse`       | Agent 调用任何工具之前         | 阻断危险操作、要求审批、修改工具输入          |
| `PostToolUse`      | 工具成功完成之后               | 运行格式化工具、记录结果、触发后续操作        |
| `PreCompact`       | 对话上下文被压缩之前           | 导出重要上下文、在截断前保存状态              |
| `SubagentStart`    | 子 Agent 被创建时              | 追踪嵌套 Agent 使用情况、初始化子 Agent 资源  |
| `SubagentStop`     | 子 Agent 完成时                | 汇总结果、清理子 Agent 资源                   |
| `Stop`             | Agent 会话结束时               | 生成报告、清理资源、发送通知                  |

## 配置钩子

钩子通过存储在工作区或用户目录中的 JSON 文件进行配置。

### 钩子文件位置

VS Code 在以下位置搜索钩子配置文件：

> [!TIP]
> 在 monorepo 中，启用 `setting(chat.useCustomizationsInParentRepositories)` 可从父仓库根目录发现钩子。了解更多关于[父仓库发现](/docs/copilot/customization/overview.md#parent-repository-discovery)的信息。

| 作用域                       | 默认文件位置                                                                 |
| ---------------------------- | ---------------------------------------------------------------------------- |
| 工作区                       | `.github/hooks/*.json`                                                       |
| 工作区（Claude 格式）        | `.claude/settings.json`、`.claude/settings.local.json`                       |
| 用户                         | `~/.copilot/hooks`、`~/.claude/settings.json`                                |
| 自定义 Agent                 | `.agent.md` frontmatter 中的 `hooks` 字段（参见 [Agent 作用域钩子](#agentscoped-hooks)） |
| 插件                         | `hooks.json` 或 `hooks/hooks.json`，具体取决于插件格式（参见[插件中的钩子](/docs/copilot/customization/agent-plugins.md#hooks-in-plugins)） |

对于相同事件类型，工作区钩子优先于用户钩子。

使用 `setting(chat.hookFilesLocations)` 设置可自定义加载哪些钩子文件。可以指定文件夹路径（VS Code 会加载该文件夹中所有 `*.json` 文件）或直接指定单个 `.json` 文件的路径。仅支持相对路径和波浪号（`~`）路径。

默认值包含以下位置：

```json
"chat.hookFilesLocations": {
  ".github/hooks": true,
  ".claude/settings.local.json": true,
  ".claude/settings.json": true,
  "~/.claude/settings.json": true
}
```

要添加自定义位置，在设置中添加条目：

```json
"chat.hookFilesLocations": {
  "custom/hooks": true,
  "~/my-hooks/security.json": true
}
```

将路径设为 `false` 可禁用从该位置加载钩子（包括默认位置）。例如，要停止从 Claude Code 配置文件加载钩子：

```json
"chat.hookFilesLocations": {
  ".claude/settings.json": false,
  ".claude/settings.local.json": false,
  "~/.claude/settings.json": false
}
```

### Agent 作用域钩子

> [!NOTE]
> Agent 作用域钩子目前处于预览阶段。

你可以直接在[自定义 Agent](/docs/copilot/customization/custom-agents.md) 的 YAML frontmatter 中定义钩子。Agent 作用域钩子仅在该自定义 Agent 处于活动状态时运行（无论是用户选择还是作为子 Agent 调用）。Agent 作用域钩子在同一事件的工作区或用户级别钩子之外额外运行。

要启用 Agent 作用域钩子，将 `setting(chat.useCustomAgentHooks)` 设为 `true`。

在 Agent frontmatter 中添加 `hooks` 字段，其结构与钩子配置文件相同：事件名称映射到钩子命令对象数组。

```markdown
---
name: "Strict Formatter"
description: "Agent that auto-formats code after every edit"
hooks:
  PostToolUse:
    - type: command
      command: "./scripts/format-changed-files.sh"
---

You are a code editing agent. After making changes, files are automatically formatted.
```

### 钩子配置格式

创建一个包含 `hooks` 对象的 JSON 文件，其中为每种事件类型包含钩子命令数组。VS Code 使用与 Claude Code 和 Copilot CLI 相同的钩子格式以保持兼容性：

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "./scripts/validate-tool.sh",
        "timeout": 15
      }
    ],
    "PostToolUse": [
      {
        "type": "command",
        "command": "npx prettier --write \"$TOOL_INPUT_FILE_PATH\""
      }
    ]
  }
}
```

### 钩子命令属性

每个钩子条目必须包含 `type: "command"` 以及至少一个命令属性：

| 属性       | 类型   | 说明                                 |
| ---------- | ------ | ------------------------------------ |
| `type`     | string | 必须为 `"command"`                   |
| `command`  | string | 默认运行的命令（跨平台）             |
| `windows`  | string | Windows 专用命令（覆盖默认值）       |
| `linux`    | string | Linux 专用命令（覆盖默认值）         |
| `osx`      | string | macOS 专用命令（覆盖默认值）         |
| `cwd`      | string | 工作目录（相对于仓库根目录）         |
| `env`      | object | 附加环境变量                         |
| `timeout`  | number | 超时时间（秒，默认值：30）           |

> [!NOTE]
> 系统专用命令根据扩展宿主平台选择。在远程开发场景（SSH、容器、WSL）中，这可能与本地操作系统不同。

### 系统专用命令

为不同操作系统指定不同命令：

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "type": "command",
        "command": "./scripts/format.sh",
        "windows": "powershell -File scripts\\format.ps1",
        "linux": "./scripts/format-linux.sh",
        "osx": "./scripts/format-mac.sh"
      }
    ]
  }
}
```

执行服务根据操作系统选择合适的命令。如果未定义系统专用命令，则回退到 `command` 属性。

## 钩子的输入与输出

钩子通过 stdin（输入）和 stdout（输出）使用 JSON 与 VS Code 通信。

### 通用输入字段

每个钩子通过 stdin 接收包含以下通用字段的 JSON 对象：

```json
{
  "timestamp": "2026-02-09T10:30:00.000Z",
  "cwd": "/path/to/workspace",
  "sessionId": "session-identifier",
  "hookEventName": "PreToolUse",
  "transcript_path": "/path/to/transcript.json"
}
```

### 通用输出格式

钩子可通过 stdout 返回 JSON 以影响 Agent 行为。所有钩子都支持以下输出字段：

```json
{
  "continue": true,
  "stopReason": "Security policy violation",
  "systemMessage": "Unit tests failed"
}
```

| 字段            | 类型    | 说明                                                         |
| --------------- | ------- | ------------------------------------------------------------ |
| `continue`      | boolean | 设为 `false` 可停止处理（默认值：`true`）                    |
| `stopReason`    | string  | 停止原因，当 `continue` 为 `false` 时使用（展示给用户）      |
| `systemMessage` | string  | 向用户展示的警告信息                                         |

### 退出码

钩子的退出码决定 VS Code 如何处理结果：

| 退出码  | 行为                                               |
| ------- | -------------------------------------------------- |
| `0`     | 成功：将 stdout 解析为 JSON                        |
| `2`     | 阻断性错误：停止处理并向模型显示错误               |
| 其他    | 非阻断性警告：向用户显示警告，继续处理             |

### 选择数据返回方式

钩子有多种控制 Agent 行为的方式：退出码、顶层输出字段（`continue`、`stopReason`）和钩子专用输出字段（`hookSpecificOutput`）。组合使用方式如下：

* **退出码 2** 是阻断操作最简单的方式。钩子的 stderr 会作为上下文显示给模型，无需 JSON 输出。
* **`continue: false`** 会停止整个 Agent 会话。使用 `stopReason` 告知用户原因。这比阻断单次工具调用更激进。
* **`hookSpecificOutput`** 提供针对每个钩子事件的精细控制。例如，`PreToolUse` 钩子使用 `permissionDecision` 来允许、拒绝或提示单次工具调用，而无需停止会话。
* **`systemMessage`** 在聊天中向用户显示警告，不受其他决策影响。

当多种控制机制同时使用时，限制最严格的优先生效。例如，若钩子同时返回 `continue: false` 和 `permissionDecision: "allow"`，会话仍会停止。

## PreToolUse

`PreToolUse` 钩子在 Agent 调用工具之前触发。

### PreToolUse 输入

除通用字段外，`PreToolUse` 钩子还会收到：

```json
{
  "tool_name": "editFiles",
  "tool_input": { "files": ["src/main.ts"] },
  "tool_use_id": "tool-123"
}
```

### PreToolUse 输出

`PreToolUse` 钩子可通过 `hookSpecificOutput` 对象控制工具执行：

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Destructive command blocked by policy",
    "updatedInput": { "files": ["src/safe.ts"] },
    "additionalContext": "User has read-only access to production files"
  }
}
```

| 字段                        | 可选值                         | 说明                     |
| --------------------------- | ------------------------------ | ------------------------ |
| `permissionDecision`        | `"allow"`、`"deny"`、`"ask"`   | 控制工具审批             |
| `permissionDecisionReason`  | string                         | 向用户展示的原因         |
| `updatedInput`              | object                         | 修改后的工具输入（可选） |
| `additionalContext`         | string                         | 给模型的额外上下文       |

**权限决策优先级**：当多个钩子针对同一工具调用运行时，限制最严格的决策优先生效：

1. `deny`（最严格）：阻断工具执行
2. `ask`：要求用户确认
3. `allow`（最宽松）：自动批准执行

**`updatedInput` 格式**：要确定 `updatedInput` 的格式，请打开 [Agent 日志](/docs/copilot/chat/chat-debug-view.md#agent-debug-log-panel)并查找记录的工具 Schema。若 `updatedInput` 与预期 Schema 不匹配，将被忽略。

## PostToolUse

`PostToolUse` 钩子在工具成功完成后触发。

### PostToolUse 输入

除通用字段外，`PostToolUse` 钩子还会收到：

```json
{
  "tool_name": "editFiles",
  "tool_input": { "files": ["src/main.ts"] },
  "tool_use_id": "tool-123",
  "tool_response": "File edited successfully"
}
```

### PostToolUse 输出

`PostToolUse` 钩子可向模型提供额外上下文，或阻断后续处理：

```json
{
  "decision": "block",
  "reason": "Post-processing validation failed",
  "hookSpecificOutput": {
    "hookEventName": "PostToolUse",
    "additionalContext": "The edited file has lint errors that need to be fixed"
  }
}
```

| 字段                                   | 可选值     | 说明                               |
| -------------------------------------- | ---------- | ---------------------------------- |
| `decision`                             | `"block"`  | 阻断后续处理（可选）               |
| `reason`                               | string     | 阻断原因（显示给模型）             |
| `hookSpecificOutput.additionalContext` | string     | 注入对话的额外上下文               |

## UserPromptSubmit

`UserPromptSubmit` 钩子在用户提交提示词时触发。

### UserPromptSubmit 输入

除通用字段外，`UserPromptSubmit` 钩子还会收到包含用户提交文本的 `prompt` 字段。

`UserPromptSubmit` 钩子仅使用通用输出格式。

## SessionStart

`SessionStart` 钩子在新 Agent 会话开始时触发。

### SessionStart 输入

除通用字段外，`SessionStart` 钩子还会收到：

```json
{
  "source": "new"
}
```

| 字段     | 类型   | 说明                                   |
| -------- | ------ | -------------------------------------- |
| `source` | string | 会话启动方式，目前始终为 `"new"`。      |

### SessionStart 输出

`SessionStart` 钩子可向 Agent 对话注入额外上下文：

```json
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "Project: my-app v2.1.0 | Branch: main | Node: v20.11.0"
  }
}
```

| 字段                | 类型   | 说明                         |
| ------------------- | ------ | ---------------------------- |
| `additionalContext` | string | 添加到 Agent 对话的上下文    |

## Stop

`Stop` 钩子在 Agent 会话结束时触发。当作用于自定义 Agent 时，`Stop` 钩子也被视为 `SubagentStop`。

### Stop 输入

除通用字段外，`Stop` 钩子还会收到：

```json
{
  "stop_hook_active": false
}
```

| 字段               | 类型    | 说明                                                                     |
| ------------------ | ------- | ------------------------------------------------------------------------ |
| `stop_hook_active` | boolean | 当 Agent 已因前一个 Stop 钩子而继续运行时为 `true`。检查此值以防止 Agent 无限运行。 |

### Stop 输出

`Stop` 钩子可阻止 Agent 停止：

```json
{
  "hookSpecificOutput": {
    "hookEventName": "Stop",
    "decision": "block",
    "reason": "Run the test suite before finishing"
  }
}
```

| 字段       | 可选值     | 说明                                                         |
| ---------- | ---------- | ------------------------------------------------------------ |
| `decision` | `"block"`  | 阻止 Agent 停止                                              |
| `reason`   | string     | 当 decision 为 `"block"` 时必填，告知 Agent 继续运行的原因。 |

> [!IMPORTANT]
> 当 `Stop` 钩子阻止 Agent 停止时，Agent 将继续运行，额外的轮次会消耗[高级请求额度](https://docs.github.com/en/copilot/managing-copilot/monitoring-usage-and-entitlements/about-premium-requests)。务必检查 `stop_hook_active` 字段以防止 Agent 无限运行。

## SubagentStart

`SubagentStart` 钩子在子 Agent 被创建时触发。

### SubagentStart 输入

除通用字段外，`SubagentStart` 钩子还会收到：

```json
{
  "agent_id": "subagent-456",
  "agent_type": "Plan"
}
```

| 字段         | 类型   | 说明                                                                  |
| ------------ | ------ | --------------------------------------------------------------------- |
| `agent_id`   | string | 子 Agent 的唯一标识符                                                 |
| `agent_type` | string | Agent 名称（例如，内置 Agent 为 `"Plan"`，或自定义 Agent 名称）       |

### SubagentStart 输出

`SubagentStart` 钩子可向子 Agent 对话注入额外上下文：

```json
{
  "hookSpecificOutput": {
    "hookEventName": "SubagentStart",
    "additionalContext": "This subagent should follow the project coding guidelines"
  }
}
```

| 字段                | 类型   | 说明                           |
| ------------------- | ------ | ------------------------------ |
| `additionalContext` | string | 添加到子 Agent 对话的上下文    |

## SubagentStop

`SubagentStop` 钩子在子 Agent 完成时触发。

### SubagentStop 输入

除通用字段外，`SubagentStop` 钩子还会收到：

```json
{
  "agent_id": "subagent-456",
  "agent_type": "Plan",
  "stop_hook_active": false
}
```

| 字段               | 类型    | 说明                                                                                 |
| ------------------ | ------- | ------------------------------------------------------------------------------------ |
| `agent_id`         | string  | 子 Agent 的唯一标识符                                                                |
| `agent_type`       | string  | Agent 名称（例如，内置 Agent 为 `"Plan"`，或自定义 Agent 名称）                       |
| `stop_hook_active` | boolean | 当子 Agent 已因前一个 Stop 钩子而继续运行时为 `true`。检查此值以防止子 Agent 无限运行。 |

### SubagentStop 输出

`SubagentStop` 钩子可阻止子 Agent 停止：

```json
{
  "decision": "block",
  "reason": "Verify subagent results before completing"
}
```

| 字段       | 可选值     | 说明                                                               |
| ---------- | ---------- | ------------------------------------------------------------------ |
| `decision` | `"block"`  | 阻止子 Agent 停止                                                  |
| `reason`   | string     | 当 decision 为 `"block"` 时必填，告知子 Agent 继续运行的原因。     |

## PreCompact

`PreCompact` 钩子在对话上下文被压缩之前触发。

### PreCompact 输入

除通用字段外，`PreCompact` 钩子还会收到：

```json
{
  "trigger": "auto"
}
```

| 字段      | 类型   | 说明                                                                  |
| --------- | ------ | --------------------------------------------------------------------- |
| `trigger` | string | 触发压缩的方式。对话超出提示词预算时为 `"auto"`。                      |

`PreCompact` 钩子仅使用通用输出格式。

## 通过 UI 配置钩子

你可以通过交互式 UI 以多种方式配置钩子：

* 在聊天输入框中输入 `/hooks` 并按 `kbstyle(Enter)`。
* 打开命令面板（`kb(workbench.action.showCommands)`）并运行 **Chat: Configure Hooks**。
* 选择聊天视图顶部的**设置**图标（<i class="codicon codicon-gear"></i>），然后选择 **Hooks**。

在钩子配置菜单中：

1. 从列表中选择一个钩子事件类型。

2. 选择现有钩子进行编辑，或选择 **Add new hook** 创建新钩子。

3. 选择或创建钩子配置文件。

该命令会在编辑器中打开钩子文件，并将光标定位在 command 字段，等待编辑。

### 使用 AI 生成钩子

你可以使用 AI 生成钩子配置。在聊天中输入 `/create-hook` 并描述你需要的自动化功能（例如，"每次文件编辑后运行 ESLint"）。Agent 会提出澄清性问题，并生成包含适当事件类型、命令和设置的钩子配置文件。

## 使用场景

以下示例演示了常见的钩子模式。

<details>
<summary>阻断危险终端命令</summary>

创建一个 `PreToolUse` 钩子以阻止破坏性命令：

**.github/hooks/security.json**：
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "./scripts/block-dangerous.sh",
        "timeoutSec": 5
      }
    ]
  }
}
```

**scripts/block-dangerous.sh**：
```bash
#!/bin/bash
INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name')
TOOL_INPUT=$(echo "$INPUT" | jq -r '.tool_input')

if [ "$TOOL_NAME" = "runTerminalCommand" ]; then
  COMMAND=$(echo "$TOOL_INPUT" | jq -r '.command // empty')

  if echo "$COMMAND" | grep -qE '(rm\s+-rf|DROP\s+TABLE|DELETE\s+FROM)'; then
    echo '{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"Destructive command blocked by security policy"}}'
    exit 0
  fi
fi

echo '{"continue":true}'
```

</details>

<details>
<summary>编辑后自动格式化代码</summary>

在任何文件修改后自动运行 Prettier：

**.github/hooks/formatting.json**：
```json
{
  "hooks": {
    "PostToolUse": [
      {
        "type": "command",
        "command": "./scripts/format-changed-files.sh",
        "windows": "powershell -File scripts\\format-changed-files.ps1",
        "timeout": 30
      }
    ]
  }
}
```

**scripts/format-changed-files.sh**：
```bash
#!/bin/bash
INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name')

if [ "$TOOL_NAME" = "editFiles" ] || [ "$TOOL_NAME" = "createFile" ]; then
  FILES=$(echo "$INPUT" | jq -r '.tool_input.files[]? // .tool_input.path // empty')

  for FILE in $FILES; do
    if [ -f "$FILE" ]; then
      npx prettier --write "$FILE" 2>/dev/null
    fi
  done
fi

echo '{"continue":true}'
```

</details>

<details>
<summary>记录工具使用日志用于审计</summary>

创建所有工具调用的审计追踪：

**.github/hooks/audit.json**：
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "./scripts/log-tool-use.sh",
        "env": {
          "AUDIT_LOG": ".github/hooks/audit.log"
        }
      }
    ]
  }
}
```

**scripts/log-tool-use.sh**：
```bash
#!/bin/bash
INPUT=$(cat)
TIMESTAMP=$(echo "$INPUT" | jq -r '.timestamp')
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name')
SESSION_ID=$(echo "$INPUT" | jq -r '.sessionId')

echo "[$TIMESTAMP] Session: $SESSION_ID, Tool: $TOOL_NAME" >> "${AUDIT_LOG:-audit.log}"
echo '{"continue":true}'
```

</details>

<details>
<summary>对特定工具要求审批</summary>

对修改基础设施的工具强制要求手动确认：

**.github/hooks/approval.json**：
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "./scripts/require-approval.sh"
      }
    ]
  }
}
```

**scripts/require-approval.sh**：
```bash
#!/bin/bash
INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name')

# 需要始终要求审批的工具
SENSITIVE_TOOLS="runTerminalCommand|deleteFile|pushToGitHub"

if echo "$TOOL_NAME" | grep -qE "^($SENSITIVE_TOOLS)$"; then
  echo '{"hookSpecificOutput":{"permissionDecision":"ask","permissionDecisionReason":"This operation requires manual approval"}}'
else
  echo '{"hookSpecificOutput":{"permissionDecision":"allow"}}'
fi
```

</details>

<details>
<summary>在会话开始时注入项目上下文</summary>

在会话开始时提供项目特定信息：

**.github/hooks/context.json**：
```json
{
  "hooks": {
    "SessionStart": [
      {
        "type": "command",
        "command": "./scripts/inject-context.sh"
      }
    ]
  }
}
```

**scripts/inject-context.sh**：
```bash
#!/bin/bash
PROJECT_INFO=$(cat package.json 2>/dev/null | jq -r '.name + " v" + .version' || echo "Unknown project")
BRANCH=$(git branch --show-current 2>/dev/null || echo "unknown")

cat <<EOF
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "Project: $PROJECT_INFO | Branch: $BRANCH | Node: $(node -v 2>/dev/null || echo 'not installed')"
  }
}
EOF
```

</details>

## 安全注意事项

如果 Agent 能够访问并编辑钩子运行的脚本，则它在运行期间有能力修改这些脚本并执行其写入的代码。建议使用 `chat.tools.edits.autoApprove` 来禁止 Agent 在未经手动批准的情况下编辑钩子脚本。

## 故障排查

### 查看钩子诊断信息

要查看已加载的钩子并检查配置错误：

1. 选择 **View Logs** 查看所有日志。

2. 查找"Load Hooks"以查看已加载的钩子及其加载来源位置。

### 查看钩子输出

要查看钩子输出和错误信息：

1. 打开**输出**面板。

2. 从通道列表中选择 **GitHub Copilot Chat Hooks**。

### 常见问题

**钩子未执行**：确认钩子文件位于 `.github/hooks/` 目录下且具有 `.json` 扩展名。检查 `type` 属性是否设为 `"command"`。

**权限拒绝错误**：确保钩子脚本具有执行权限（`chmod +x script.sh`）。

**超时错误**：增大 `timeout` 值或优化钩子脚本，默认超时时间为 30 秒。

**JSON 解析错误**：确认钩子脚本向 stdout 输出有效的 JSON。使用 `jq` 或 JSON 库来构建输出。

## 常见问题解答

### VS Code 如何处理 Claude Code 钩子配置？

VS Code 默认从 `.claude/settings.json`、`.claude/settings.local.json` 和 `~/.claude/settings.json` 读取钩子配置。VS Code 解析 Claude Code 的钩子配置格式，包括匹配器语法。目前 VS Code 忽略匹配器值，因此钩子会在所有工具调用时运行，不受匹配器中工具名称的限制。

将 Claude Code 钩子迁移到 VS Code 时，需注意以下差异：

* **工具输入属性名**：Claude Code 的工具输入属性使用 snake_case（例如 `tool_input.file_path`），而 VS Code 工具使用 camelCase（例如 `tool_input.filePath`）。需要更新钩子脚本以读取正确的属性名。
* **工具名称**：Claude Code 与 VS Code 使用不同的工具名称。例如，Claude Code 使用 `Write` 和 `Edit` 进行文件操作，而 VS Code 使用 `create_file` 和 `replace_string_in_file` 等工具名称。需在 `tool_name` 输入字段中检查工具名称并相应更新钩子逻辑。
* **匹配器被忽略**：`"Edit|Write"` 等钩子匹配器会被解析但不会被应用。无论匹配器中的工具名称如何，所有钩子都会在每个匹配事件上运行。

### VS Code 如何处理 Copilot CLI 钩子配置？

VS Code 会解析 Copilot CLI 的钩子配置，并将 lowerCamelCase 格式的钩子事件名称（如 `preToolUse`）转换为 VS Code 使用的 PascalCase 格式（`PreToolUse`）。`bash` 和 `powershell` 命令属性会映射到系统专用命令：`powershell` 映射到 `windows`，`bash` 映射到 `osx` 和 `linux`。

## 安全事项

> [!CAUTION]
> 钩子以与 VS Code 相同的权限执行 Shell 命令。使用来自不受信任来源的钩子时，请仔细审查钩子配置。

* **审查钩子脚本**：在共享仓库中，启用前请检查所有钩子脚本。

* **限制钩子权限**：遵循最小权限原则，钩子只应拥有其所需的访问权限。

* **验证输入**：钩子脚本从 Agent 接收输入，验证并净化所有输入以防止注入攻击。

* **保护凭据**：切勿在钩子脚本中硬编码密钥，使用环境变量或安全凭据存储。

## 相关资源

* [在 Agent 中使用工具](/docs/copilot/agents/agent-tools.md) — 了解工具审批和执行
* [自定义 Agent](/docs/copilot/customization/custom-agents.md) — 创建专用 Agent 配置
* [子 Agent](/docs/copilot/agents/subagents.md) — 将任务委托给独立上下文的子 Agent
* [安全注意事项](/docs/copilot/security.md) — VS Code 中 AI 安全的最佳实践