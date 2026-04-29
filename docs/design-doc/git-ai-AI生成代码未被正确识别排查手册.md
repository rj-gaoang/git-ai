# git-ai AI 生成代码未被正确识别排查手册

## 1. 适用场景

这份文档用于排查下面这类问题：

1. 代码明明是 AI 生成的，但 `git-ai status`、`git-ai blame` 或统计结果里没有体现为 AI。
2. 开发者确认自己使用了 Copilot、Cursor、Windsurf 或其他接入了 `git-ai` 的工具，但提交后仍然看起来像“纯人工代码”。
3. 同一份代码在一台机器上能看到 AI 归因，在另一台机器上却看不到。

先把边界说清楚：`git-ai` 不会靠 diff 去“猜”哪些代码像 AI 写的。它依赖接入的工具在编辑时触发 `git-ai checkpoint`，把实际由 AI 写入的行记录下来，然后在 commit 后写入 `refs/notes/ai`。

这意味着，只要 checkpoint 没产生、产生错了、或者 notes 没同步到当前机器，最终都会表现成“AI 代码没有被正确识别”。

---

## 2. 先记住 5 个结论

1. `git-ai` 识别 AI 代码的前提不是“代码看起来像 AI 写的”，而是编辑阶段真的产生了 AI checkpoint。
2. 只有 `AiAgent` 和 `AiTab` checkpoint 会被当成 AI；`Human` 和 `KnownHuman` 不会。
3. `git-ai status` 看的其实是“自上次 commit 以来的 working log 和 checkpoint”，所以它是排查未提交 AI 识别问题的第一入口。
4. 已提交代码的 AI 归因最终落在 `refs/notes/ai`；如果当前机器没有这份 notes，本地看起来就会像“没有识别”。
5. `prompt_storage` 主要影响 prompt 正文和 messages 是否保存在 note 里，不决定一行代码是不是 AI 生成的。

代码依据：

1. README 明确说明 hooks 会调用 `git-ai checkpoint` 来建立 AI 行与模型、prompt 的关联。
2. `git-ai/src/authorship/working_log.rs` 里只有 `AiAgent` 和 `AiTab` 被视为 AI checkpoint。
3. `git-ai/src/commands/status.rs` 会读取 working log 和 checkpoints，并在没有 checkpoint 时直接提示检查 hooks。
4. `git-ai/src/git/sync_authorship.rs` 说明已提交归因存储在 `refs/notes/ai`，并支持从 remote 拉取。

---

## 3. 最短排查路径

如果是“刚生成、还没 commit 的代码”，先执行：

```powershell
git-ai status --json
```

如果是“已经 commit 的代码”，先执行：

```powershell
git-ai blame <文件路径>
git notes --ref=ai show HEAD
```

如果怀疑是当前机器缺少 notes，再执行：

```powershell
git-ai fetch-notes
git-ai blame <文件路径>
```

如果怀疑只是 prompt 信息不完整，而不是 AI 归因本身丢失，再执行：

```powershell
git-ai config prompt_storage
```

判定方法：

1. `git-ai status --json` 里没有任何 AI checkpoint：优先查 hooks、IDE 接入和 agent 触发。
2. `git-ai status --json` 有 checkpoint，但全是 human：优先查 checkpoint 类型为什么没记成 AI。
3. `git-ai blame` 看不到 AI，但 `git-ai fetch-notes` 后恢复：问题在 notes 同步，不在识别算法。
4. `git-ai blame` 能看到 AI，但 prompt 或 messages 不全：问题通常是 `prompt_storage`，不是 AI 识别丢失。

---

## 4. 按故障层分段排查

### 4.1 第一层：根本没有产生 AI checkpoint

这是最常见的根因。

先执行：

```powershell
git-ai status --json
```

如果结果里没有 checkpoint，或者命令直接提示：

```text
No checkpoints recorded since last commit
```

说明当前工作区里没有任何能证明“这段代码是 AI 写的”的记录。

优先检查：

```powershell
git-ai install-hooks
git-ai debug
git config --show-origin --get core.hooksPath
```

如果使用 VS Code / GitHub Copilot，还要额外检查 native hook 是否被 VS Code 加载：

```powershell
Get-Content "$HOME\.copilot\hooks\git-ai.json"
Get-Content "$env:APPDATA\Code\User\settings.json"
```

重点看 `settings.json` 里的 `chat.hookFilesLocations`。如果这个配置被用户手动覆盖，且里面没有 `~/.copilot/hooks`，即使 `git-ai install-hooks` 已经生成了 `$HOME\.copilot\hooks\git-ai.json`，VS Code 也可能不会加载这份 hook，结果就是 Copilot 编辑不会触发 `git-ai checkpoint`。

推荐配置至少包含：

```json
"chat.hookFilesLocations": {
	"~/.copilot/hooks": true,
	"~/.github/hooks": true
}
```

重点看：

1. 当前仓库的 hooks 是否真的安装到了生效的 `core.hooksPath`。
2. 当前 IDE / agent 是否属于已接入 `git-ai` checkpoint 的那条链路。
3. 这段代码是不是通过“复制粘贴 AI 回复”写进去的，而不是通过接入了 `git-ai` 的编辑链路落地。

这里要特别注意：如果代码是人工从聊天窗口复制到编辑器里，再手动保存，`git-ai` 不会事后猜出它是 AI 生成的。

---

### 4.2 第二层：VS Code hook 已执行，但没有提取到本次编辑路径

这个场景很容易被误判成“hook 没加载”。实际特征通常是：

1. GitHub Copilot Chat hook 日志里 `PreToolUse` / `PostToolUse` 显示 `Success`，而且 `cwd` 是目标仓库。
2. `tool_name` 是 `apply_patch`、`create_file`、`edit_file` 这类编辑工具。
3. `.git\ai\working_logs\<base_commit>\checkpoints.jsonl` 里只有 `KnownHuman`，或者完全没有 `AiAgent`。
4. hook 日志里的 `tool_input` / `tool_response` 显示为 `"..."`，或者没有任何可提取的文件路径。

建议先看最近的 Copilot hook 日志：

```powershell
Get-ChildItem "$env:APPDATA\Code\logs" -Recurse -Filter "GitHub Copilot Chat Hooks.log" |
	Sort-Object LastWriteTime -Descending |
	Select-Object -First 1 |
	Get-Content -Tail 200
```

再看当前仓库最近的 working log：

```powershell
Get-ChildItem ".git\ai\working_logs" -Recurse -Filter checkpoints.jsonl |
	Sort-Object LastWriteTime -Descending |
	Select-Object -First 5 |
	ForEach-Object { $_.FullName; Get-Content $_.FullName -Tail 20 }
```

如果确认 hook 已经执行，但没有 `AiAgent`，重点就不是重复检查 `chat.hookFilesLocations`，而是看本次 hook payload 里有没有可用编辑路径。

新版 `git-ai` 对 GitHub Copilot native hook 增加了一个安全兜底：当 `tool_input` / `tool_response` 提不出路径时，会用当前 hook 的 `tool_use_id` / `toolCallId` 精确回查 `transcript_path` 里的同一个工具调用参数，再从 `apply_patch` 等参数中提取文件路径。这个兜底只匹配同一次 tool call，不会扫描整段会话历史，避免把其他工具调用的文件错误归到当前 checkpoint。

如果旧版本出现这个现象，处理方式是升级到包含该兜底的 `git-ai`，然后重新做一次受控 AI 编辑验证；已经提交且没有 `AiAgent` checkpoint 的历史 commit，不能靠 `git-ai` 事后从代码风格自动补判。

---

### 4.3 第三层：有 checkpoint，但被记录成了 human

`git-ai` 的 checkpoint 有 4 种类型：

1. `Human`
2. `AiAgent`
3. `AiTab`
4. `KnownHuman`

其中只有 `AiAgent` 和 `AiTab` 会被算成 AI。

因此，如果 `git-ai status --json` 能看到 checkpoints，但这些 checkpoint 对应的是 human 或 known_human，那么最终结果仍然会表现成“AI 没被识别”。

建议这样看：

```powershell
git-ai status --json
```

重点关注输出里的：

1. `checkpoints`
2. `tool_model`
3. `is_human`

如果现场看到的 checkpoint 基本都是当前用户名，或者 `is_human=true`，而没有明确的 AI tool/model，那么问题不是后面的 commit 逻辑，而是编辑阶段就没有把这次写入记成 AI。

常见原因：

1. IDE 只触发了人类保存事件，没有触发 AI 写入事件。
2. 接入链路只做了 human checkpoint，没有带 agent metadata。
3. AI 先生成了代码，但后续被人类大幅重写，最终留下来的行已经不再属于最初的 AI checkpoint。

---

### 4.4 第四层：commit 已经带了 AI 归因，但当前机器没有 notes

这类问题最容易被误判成“没识别”。

表现通常是：

1. 其他同事能看到这份代码带 AI 归因。
2. 当前机器执行 `git-ai blame <文件路径>` 却看不到 AI。

优先检查：

```powershell
git notes --ref=ai list
git notes --ref=ai show HEAD
```

如果本地 `refs/notes/ai` 很少、为空，或者目标 commit 没 note，继续执行：

```powershell
git-ai fetch-notes
```

再重新验证：

```powershell
git-ai blame <文件路径>
```

这类问题的本质不是 AI 没被识别，而是存放归因结果的 notes 还没同步到当前仓库。

---

### 4.5 第五层：AI 归因在，但 prompt 信息不完整

这类问题经常和“未识别”混在一起，但它们不是一回事。

如果你已经能在 `git-ai blame` 里看到 AI 作者，或者 note 里已经有 AI attribution，只是：

1. prompt 正文为空
2. messages 没带出来
3. 上传到服务端后只看到 AI 行数，看不到 promptText

优先检查：

```powershell
git-ai config prompt_storage
```

说明：

1. `prompt_storage=notes` 时，prompt messages 会保留在 git note 里。
2. `prompt_storage=default` 或 `local` 时，可能仍然能识别这段代码属于 AI，但 note / 上传链路里拿不到完整 prompt 正文。

所以：

1. “AI 代码没识别”看的是 checkpoint 和 notes。
2. “prompt 信息没带出来”看的是 `prompt_storage`。

不要把这两个问题混成一个问题排查。

---

## 5. 最常见的判断矩阵

| 现场现象 | 最可能的问题层 | 先做什么 |
| --- | --- | --- |
| `git-ai status --json` 没有任何 checkpoint | 没有产生 AI checkpoint | 查 hooks、IDE 接入、是否只是手动粘贴 AI 代码 |
| VS Code hook 日志显示 `Success`，但 working log 只有 `KnownHuman` 或没有 `AiAgent` | hook payload 没有可用编辑路径 | 查 `tool_input`、`tool_use_id`、`transcript_path`，升级到含 transcript 精确兜底的版本 |
| 有 checkpoints，但都是 human | checkpoint 类型错了 | 查 `is_human`、`tool_model`，回到编辑链路 |
| 某台机器看不到 AI，别的机器能看到 | 本地缺少 `refs/notes/ai` | 先跑 `git-ai fetch-notes` |
| `git-ai blame` 能看到 AI，但 prompt 为空 | `prompt_storage` 问题 | 查 `git-ai config prompt_storage` |
| 刚生成时像 AI，改几轮后看起来变成人工 | 后续人类改写覆盖了原归因 | 对比更早 commit 或更早 blame 结果 |

---

## 6. 建议现场一次性回传的证据

为了避免来回追问，建议让出问题的同事一次性回传这些输出：

```powershell
git-ai status --json
git-ai blame <文件路径>
git notes --ref=ai list
git notes --ref=ai show HEAD
git-ai debug
git config --show-origin --get core.hooksPath
git-ai config prompt_storage
```

如果问题是“别人机器能看到，我这里看不到”，再补：

```powershell
git-ai fetch-notes
git notes --ref=ai list
git-ai blame <文件路径>
```

只要拿到这些信息，基本就能判断问题停在：

1. AI checkpoint 没产生
2. checkpoint 类型错误
3. commit note 没生成
4. notes 没同步到当前机器
5. prompt_storage 只影响 prompt，不影响 AI 归因

---

## 7. 代码定位

如果后续要继续追代码，优先看这些文件：

1. `git-ai/README.md`
2. `git-ai/src/authorship/working_log.rs`
3. `git-ai/src/commands/status.rs`
4. `git-ai/src/git/sync_authorship.rs`
5. `git-ai/src/config.rs`

它们分别对应：

1. checkpoint 是如何参与 AI 归因的整体说明
2. 哪些 checkpoint kind 会被视为 AI
3. 未提交状态下如何读取 checkpoints 和 working log
4. 已提交归因 notes 如何从 remote 拉到本地
5. `prompt_storage` 如何影响 prompt 保存位置
