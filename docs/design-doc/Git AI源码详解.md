# Git AI 如何判断代码是否由 GitHub Copilot 生成——源码级详解

> 本文面向不熟悉 Rust 的读者，所有代码逻辑用中文逐步拆解。
> 全部内容基于项目源码，不做推测。

---

## 目录

- [名词表：本文会用到的专有名词](#名词表本文会用到的专有名词)
- [总览：这套系统到底在干什么](#总览这套系统到底在干什么)
- [架构：两层系统](#架构两层系统)
- [第一层：VS Code 扩展——谁负责监听编辑事件](#第一层vs-code-扩展谁负责监听编辑事件)
  - [通道一：Copilot Chat 编辑（旧方案）](#通道一copilot-chat-编辑旧方案legacy-hooks)
  - [通道二：Copilot Chat 编辑（新方案）](#通道二copilot-chat-编辑新方案native-hooks)
  - [通道三：Copilot Tab 补全](#通道三copilot-tab-补全)
  - [通道四：KnownHuman——人类主动保存检查点](#通道四knownhuman人类主动保存检查点)
- [第二层：Rust 核心——怎么把编辑事件变成归因结果](#第二层rust-核心怎么把编辑事件变成归因结果)
  - [步骤一：事件路由——把 JSON 翻译成统一结构](#步骤一事件路由把-json-翻译成统一结构)
  - [步骤二：确定要处理哪些文件](#步骤二确定要处理哪些文件)
  - [步骤三：对每个文件做字符级 diff 归因](#步骤三对每个文件做字符级-diff-归因)
  - [步骤四：字符级归因聚合成行级归因](#步骤四字符级归因聚合成行级归因)
- [提交阶段：归因数据怎么持久化](#提交阶段归因数据怎么持久化)
- [完整流程图](#完整流程图)
- [附：关键数据结构一览](#附关键数据结构一览)

---

## 名词表：本文会用到的专有名词

| 名词 | 含义 |
|------|------|
| **Checkpoint（检查点）** | Git AI 的核心概念。每次有编辑发生时，系统会拍一次"快照"，把当前文件内容和归因信息记录下来。可以类比成"存档点"。 |
| **Attribution（归因）** | 回答"这段代码是谁写的"。每个归因包含：字符范围（从第几个字符到第几个字符）、作者 ID、时间戳。 |
| **LineAttribution（行级归因）** | 把字符级的归因聚合到行。回答"这一行最终算谁的"。 |
| **CheckpointKind（检查点类型）** | 四种值：`Human`（默认人工基线，AI hook 之前快照）、`AiAgent`（AI 聊天编辑）、`AiTab`（AI Tab 补全）、`KnownHuman`（VS Code 扩展在用户保存时主动声明的人类编辑，带 IDE 元数据）。 |
| **ToolClass（工具分类）** | Rust 侧对每个 Copilot 工具调用的分类：`FileEdit`（改文件）、`Bash`（运行终端命令，例如 `run_in_terminal`）、`Skip`（跳过）。不同分类走不同的 checkpoint 路径。 |
| **hook（钩子）** | 在某个事件发生前后自动触发的回调。比如"Copilot 执行工具前触发 PreToolUse 钩子"。 |
| **Preset（预设）** | Rust 侧的一个处理模块。不同 AI 工具（Copilot、Claude 等）各有一个 preset，负责解析各自的 hook 数据。 |
| **diff（差异比较）** | 把两个版本的文件内容逐字节对比，找出哪些部分被新增、删除、修改了。 |
| **dirty_files（脏文件）** | VS Code 编辑器里已修改但尚未保存、或者刚保存但磁盘可能还没刷新的文件。扩展会把这些文件的内存内容一起传给 Rust。 |
| **Working Log（工作日志）** | 存放在 `.git/ai/working_logs/` 下的文件，记录两次 git commit 之间的所有 checkpoint。commit 时会汇总清理。 |
| **git notes（git 笔记）** | git 自带的机制，可以给每个 commit 附加额外信息，不会改变 commit 本身。Git AI 用它存放最终归因结果。只要 `refs/notes/ai` 也被同步到另一台机器，另一台机器就能在这些已提交归因的基础上继续归因。 |
| **transcript（对话记录）** | Copilot Chat 的对话内容，包括用户输入、AI 回复、使用了哪些工具。存储在 VS Code 的 storage 目录下。 |
| **scheme（URI 协议）** | VS Code 里每个文档都有一个 URI，scheme 是协议部分。普通文件是 `file`，Copilot 编辑快照是 `chat-editing-snapshot-text-model`。 |
| **debounce（防抖）** | 短时间内连续发生多次相同事件时，只处理最后一次（或第一次），避免重复。 |
| **imara_diff** | 一个 Rust 语言的 diff 库，使用的算法和 git diff 相同（Myers 差异算法）。Git AI 用它做字节级差异比较。 |
| **session（会话）** | Copilot Chat 的一次对话。一个 session 有唯一的 ID，对应一个 `.jsonl` 或 `.json` 文件。 |
| **move detection（移动检测）** | diff 时发现一段文本从 A 位置移到了 B 位置。如果检测到是"移动"而非"删除+新增"，归因会跟着移动，而不是重新标记。 |

---

## 总览：这套系统到底在干什么

> **Git AI 是什么？**
>
> Git AI 是一个开源工具，目的是追踪"代码里哪些部分是 AI 生成的"。
> 它和 git（版本控制工具）配合使用，在你正常写代码的过程中自动记录。
>
> **GitHub Copilot 是什么？**
>
> GitHub Copilot 是微软/GitHub 推出的 AI 编程助手。
> 它可以在 VS Code 里帮你写代码，主要有两种方式：
> 1. **Chat 模式**：你在聊天框里说"帮我写一个排序函数"，Copilot 自动修改你的代码文件
> 2. **Tab 补全模式**：你正在打字时，Copilot 自动建议接下来的代码，你按 Tab 键接受

先说结论：

**Git AI 不是在看代码"像不像 AI 写的"，而是在编辑发生的当下，实时记录"谁在什么时间改了哪些字符"。**

它的思路是：

1. 在 AI 动手之前，先拍一张"人工基线"快照——此时文件里的内容都当作人类写的。
2. 在 AI 动手之后，再拍一张"AI 编辑结果"快照。
3. 把两张快照做 diff，精确算出哪些字符是这次 AI 改出来的。
4. 如果人类后来又改了这些行，再把归因覆盖回人类。
5. 提交 (git commit) 时，把最终归因写进 git notes。

所以它是"监控编辑过程"（类似监控摄像头），不是"事后做法医鉴定"。

---

## 架构：两层系统

> **什么是"架构"？**
>
> 架构就是"这个软件由哪些部分组成，各部分怎么分工、怎么配合"。
> 这里 Git AI 分成两部分：前端（VS Code 扩展）和后端（Rust 命令行程序）。
>
> 以下架构图描述的是**新方案（Native Hooks，VS Code ≥ 1.109.3）**。

```
┌──────────────────────────────────────────────────────────────────┐
│                                                                  │
│   第 1 层：VS Code 扩展 (TypeScript 编写)                          │
│                                                                  │
│   职责（新方案下已缩减，但仍有持续运行的部分）：                       │
│   • 【一次性】安装钩子配置：                                         │
│       写入 ~/.copilot/hooks/git-ai.json （旧路径                  │
│         ~/.github/hooks/git-ai.json 会被自动删除）                 │
│       写入 VS Code settings.json 的 "chat.useHooks": true         │
│   • 【持续运行】KnownHuman 保存检查点（KnownHumanCheckpointManager）│
│       每次 onDidSaveTextDocument，按仓库 root 防抖 500ms          │
│       触发 git-ai checkpoint known_human                          │
│   • 【持续运行】Blame Gutter 显示（BlameLensManager）               │
│   • 【持续运行】Tab 补全追踪（AITabEditManager，实验功能）            │
│   • ✗ 不再监听 Chat 编辑器快照事件、不再判断"是否 Copilot 编辑"     │
│   • ✗ 不再主动调用 git-ai CLI 传送 Chat 编辑数据（由 VS Code        │
│       Native Hooks 直接调用）                                     │
│                                                                  │
│   关键文件：                                                       │
│   • agent-support/vscode/src/extension.ts                  ← 入口 │
│   • agent-support/vscode/src/known-human-checkpoint-manager.ts    │
│   • agent-support/vscode/src/blame-lens-manager.ts  ← Gutter 展示 │
│   • agent-support/vscode/src/ai-tab-edit-manager.ts ← Tab 补全    │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
         │
         │ 安装阶段写入配置（一次性）
         ▼
┌──────────────────────────────────────────────────────────────────┐
│                                                                  │
│   第 1.5 层：VS Code 本体（内建 Chat Hooks 机制）                   │
│                                                                  │
│   职责：                                                          │
│   • 读取 ~/.copilot/hooks/git-ai.json 中的钩子配置                │
│     （为兼容旧版本，~/.github/hooks/git-ai.json 也会被识别为遗留路径）│
│   • 拦截所有 Copilot 工具调用（PreToolUse / PostToolUse）           │
│   • 自动 spawn git-ai 子进程，将工具调用信息打包为 JSON 写入 stdin   │
│                                                                  │
│   传递的数据（示例）：                                               │
│   {                                                              │
│     "hookEventName": "PostToolUse",                              │
│     "toolName": "copilot_replaceString",                         │
│     "toolInput": { "file_path": "src/main.ts" },                 │
│     "sessionId": "xxx-xxx-xxx",                                  │
│     "transcript_path": "/path/to/session.jsonl",                 │
│     "cwd": "/your/project"                                       │
│   }                                                              │
│                                                                  │
└──────────────────────────┬───────────────────────────────────────┘
                           │
                           │ stdin 传 JSON
                           │ 命令: git-ai checkpoint github-copilot --hook-input stdin
                           │
                           ▼
┌──────────────────────────────────────────────────────────────────┐
│                                                                  │
│   第 2 层：Rust 核心 (git-ai CLI)                                  │
│                                                                  │
│   职责：接收事件 → 过滤非编辑工具 → 做 diff → 计算字符级归因         │
│         → 聚合行级归因 → 存到 working log → commit 时写入 git notes │
│                                                                  │
│   关键文件：                                                       │
│   • src/commands/checkpoint_agent/agent_presets.rs  ← 事件路由    │
│   • src/commands/checkpoint.rs                      ← 主流程     │
│   • src/authorship/attribution_tracker.rs           ← diff 归因  │
│   • src/authorship/post_commit.rs                   ← 提交落盘   │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

两层之间的通信方式很简单：

- VS Code 本体启动 `git-ai` 命令行进程
- 把 JSON 数据通过 stdin（标准输入）喂给它
- Rust 程序解析 JSON、执行归因计算、写入本地存储

> **什么是 stdin（标准输入）？**
>
> 每个命令行程序天生有三个"通道"：
> - **stdin（标准输入）**：程序从这里读取数据，类似"进水管"
> - **stdout（标准输出）**：程序从这里输出结果，类似"出水管"
> - **stderr（标准错误）**：程序从这里输出错误信息
>
> 当 VS Code 触发 `git-ai checkpoint ...` 时，它会把 JSON 数据"灌"进 stdin。
> Rust 程序从 stdin 里"接住"这些数据。这种方式不需要网络、不需要文件，两个程序之间直接传数据。

---

## 第一层：VS Code 扩展——谁负责监听编辑事件

> **什么是 VS Code 扩展？**
>
> VS Code 是一个代码编辑器。它支持安装"扩展（extension）"来增加功能，
> 类似手机上安装 App。Git AI 的扩展就是这样一个"App"，
> 安装后它能在 VS Code 内部运行代码，监听各种事件。
>
> **什么是 TypeScript？**
>
> VS Code 扩展用 TypeScript 语言编写（一种增强版的 JavaScript）。
> 你不需要懂 TypeScript，本文所有逻辑都用中文描述。

Git AI 针对 Copilot 有三条独立的检测通道，分别对应 Copilot 的三种工作模式。

### 扩展启动时注册了哪些监听器

> **什么是"监听器（listener）"？**
>
> 监听器就是一段提前注册好的代码，等着某件事发生。
> 比如"当文件被保存时，执行这段代码"——这就是一个监听"文件保存"事件的监听器。
> 类似手机上设的"当时间到 7:00，响铃"——闹钟就是一个监听"时间到了"事件的监听器。

在 `extension.ts` 里（扩展入口），启动时会做这些事：

```
扩展启动 (activate)
    │
    ├─ 创建 KnownHumanCheckpointManager 实例（无论新/旧方案都启用）
    │   并注册 onDidSaveTextDocument 监听器
    │   每次保存 → 按 git 仓库根目录防抖 500ms →
    │     spawn: git-ai checkpoint known_human --hook-input stdin
    │   只处理 file:// scheme 文档，跳过 .vscode/ 内部文件
    │
    ├─ 创建 AIEditManager 实例
    │   └─ 根据 VS Code 版本决定用旧方案还是新方案
    │      如果 VS Code 版本 ≥ 1.109.3 → 用新方案（VS Code 原生钩子）
    │      如果 VS Code 版本 < 1.109.3 → 用旧方案（扩展自己监听）
    │
    ├─ 如果用旧方案（chat 编辑追踪），注册 4 个文档事件监听器：
    │   ├─ onDidSaveTextDocument    → 文件保存时触发
    │   ├─ onDidOpenTextDocument    → 文档打开时触发
    │   ├─ onDidCloseTextDocument   → 文档关闭时触发
    │   └─ onDidChangeTextDocument  → 文档内容变化时触发
    │
    ├─ 如果启用了 Tab 追踪（实验功能）：
    │   ├─ 创建 AITabEditManager 实例
    │   └─ 劫持 Tab 补全接受命令
    │
    └─ 其他功能（blame lens 等，和归因检测无关）
```

> **"为什么旧方案和新方案不能同时用？"**
>
> 因为会重复计算。如果两套都开，同一次 Copilot 编辑会被记两次。
> 所以扩展里做了互斥：版本 ≥ 1.109.3 就只用新方案，低版本才用旧方案。
> Tab 补全是独立通道，不受这个互斥影响。
> KnownHuman 保存检查点也是独立通道，始终启用；它并不重复记录 AI 编辑，
> 只是为人类保存动作补一次明确的快照，并且在 AI checkpoint 后 1 秒内发生的
> KnownHuman 会被主动丢弃（参见下文《KnownHuman：人类主动保存检查点》）。

---

### 通道一：Copilot Chat 编辑（旧方案 Legacy Hooks）

**这是最复杂的一条链路**，也是整个项目的核心设计。

#### 1.1 它到底监听了什么

旧方案的全部逻辑在 `ai-edit-manager.ts` 里。

它不是"看到代码变了就猜是不是 Copilot"。它监听的不是代码本身的变化，而是 VS Code 内部产生的一种**特殊文档**。

当你在 Copilot Chat 里说"帮我改这个函数"时，VS Code 内部会做这些事：

> **什么是 URI？**
>
> URI（统一资源标识符）是用来标识资源的一串文字，格式大致是 `协议:路径`。
> 比如你见过的网页地址 `https://www.example.com` 就是一个 URI，
> 其中 `https` 是协议（scheme），`www.example.com` 是路径。
>
> 在 VS Code 里，每个打开的文档都有一个 URI。
> 普通文件的 scheme 是 `file`，比如 `file:///C:/project/main.py`。
> 但 VS Code 内部还有很多"虚拟文档"，它们的 scheme 不是 `file`，
> 而是更复杂的名字——这就是 Git AI 检测 Copilot 编辑的关键线索。

1. 打开一个特殊的"编辑快照文档"——这不是你磁盘上的源码文件
2. 这个文档的 URI scheme 不是普通的 `file`，而是：
   - `chat-editing-snapshot-text-model` —— 编辑快照的文本模型
   - `chat-editing-text-model` —— 聊天编辑的文本模型
3. Copilot 在这个文档上做编辑
4. 编辑完成后，VS Code 把结果写回（保存）到你的真实文件

Git AI 扩展就是在监听第 2 步和第 4 步：

- 看到特殊 scheme 的文档被打开 → "Copilot 准备改文件了"
- 看到文件被保存 → "Copilot 可能改完了"

#### 1.2 从"Copilot 准备改文件"到"发出 before_edit"

当 `handleOpenEvent()` 检测到打开的文档 scheme 是 Copilot 专属的：

```
handleOpenEvent(doc) 被调用
    │
    ├─ 如果 doc.uri.scheme 是 "file"
    │   → 初始化 stableFileContent 缓存（记住当前文件内容）
    │   → 结束
    │
    └─ 如果 doc.uri.scheme 是 "chat-editing-snapshot-text-model"
       或 "chat-editing-text-model"
       │
       ├─ 记录 snapshotOpenEvents[filePath]
       │   包含：
       │   • timestamp: 当前时间
       │   • count: 出现次数（累加）
       │   • uri: 这个快照文档的完整 URI
       │
       └─ 调用 triggerHumanCheckpoint([filePath])
```

`triggerHumanCheckpoint()` 做了什么：

```
triggerHumanCheckpoint(willEditFilepaths)
    │
    ├─ 检查文件列表是否为空 → 空则跳过
    │
    ├─ 防抖过滤（HUMAN_CHECKPOINT_DEBOUNCE_MS = 500 毫秒）
    │   如果这个文件 500 毫秒内已经被 checkpoint 过 → 跳过
    │   目的：避免 Copilot 连续编辑多个文件时重复触发
    │
    ├─ 收集 dirty_files（当前编辑器里所有未保存的文件内容）
    │
    ├─ 对于即将被编辑的文件，优先取 stableFileContent 缓存
    │   │
    │   │  什么是 stableFileContent？
    │   │  ─────────────────────────
    │   │  扩展会监听文件内容变化（onDidChangeTextDocument）
    │   │  每次内容变化后，等待 2 秒静默期（STABLE_CONTENT_DEBOUNCE_MS）
    │   │  2 秒内没有新的变化，才把当前内容记为"稳定内容"
    │   │
    │   │  为什么不直接读当前文件内容？
    │   │  因为 Copilot 可能已经开始往编辑器里塞内容了
    │   │  但还没保存到磁盘。如果这时读编辑器内容
    │   │  "人工基线"就已经被 AI 污染了
    │   │  用 stableFileContent 可以拿到 AI 动手前的干净版本
    │   │
    │   └─ 如果没有缓存 → 从 VS Code 编辑器内存直接读
    │
    ├─ 找到 workspaceFolder（工作区文件夹）
    │
    └─ 调用 checkpoint("human", hookInput)
       其中 hookInput = {
         hook_event_name: "before_edit",
         workspace_folder: 工作区路径,
         will_edit_filepaths: [即将被改的文件路径],
         dirty_files: { 文件路径: 文件内容, ... }
       }
```

**到这里，扩展已经完成了"编辑前的现场拍照"。**

#### 1.3 从"文件被保存"到"发出 after_edit"

当用户的文件被保存时，`handleSaveEvent()` 被调用：

```
handleSaveEvent(doc)
    │
    ├─ 拿到文件路径
    ├─ 清除该文件之前的防抖计时器（如果有）
    ├─ 设置新的防抖计时器（300 毫秒后触发）
    │   SAVE_EVENT_DEBOUNCE_WINDOW_MS = 300
    │   目的：如果 300 毫秒内连续保存多次，只处理最后一次
    │
    └─ 300 毫秒后调用 evaluateSaveForCheckpoint(filePath)
```

`evaluateSaveForCheckpoint()` 是旧方案中**最关键的判断函数**。它不是看到保存就认为是 AI，而是连续检查一系列条件：

```
evaluateSaveForCheckpoint(filePath)
    │
    ├─ 【条件 A：必须有快照打开事件】
    │   检查 snapshotOpenEvents[filePath] 是否存在
    │   检查 count ≥ 1
    │   检查 uri.query 是否非空
    │   │
    │   如果不满足 → 跳过（"普通手工保存，不可能是 Copilot"）
    │   │
    │   为什么这个条件有效？
    │   ──────────────────
    │   只有 Copilot Chat 编辑流程会打开 chat-editing-snapshot 文档
    │   普通编辑不会产生这种文档
    │   所以如果之前没有看到快照文档被打开
    │   这次保存绝不可能是 Copilot 编辑的结果
    │
    ├─ 【条件 B：快照必须足够新——10 秒窗口】
    │   snapshotAge = 当前时间 - snapshotInfo.timestamp
    │   如果 snapshotAge ≥ 10000 毫秒（10 秒）→ 跳过
    │   │
    │   为什么设 10 秒？
    │   ──────────────
    │   防止这种场景：
    │   • 你 30 秒前看了一眼 Copilot 的编辑预览（产生了快照打开事件）
    │   • 你取消了编辑
    │   • 你自己手工改了文件并保存
    │   • 如果不设时间限制，这次手工保存会被误记成 Copilot 编辑
    │   10 秒是经验值：正常的 Copilot 编辑流程（打开快照→Copilot改→保存）
    │   通常在几秒内完成
    │
    ├─ 【条件 C：必须能从快照 URI 解析出 sessionId】
    │   从 snapshotInfo.uri.query 里解析 JSON 参数
    │   │
    │   什么是 uri.query？
    │   ─────────────────
    │   URI 可以包含额外参数，放在 ? 后面。
    │   比如 https://example.com/search?keyword=hello 里
    │   "keyword=hello" 就是 query 部分。
    │   Copilot 的快照 URI 的 query 里塞了 JSON 格式的参数
    │   包含会话 ID 等信息。
    │   │
    │   尝试以下字段（按优先级）：
    │   1. params.chatSessionId
    │   2. params.sessionId
    │   3. params.chatSessionResource.path → Base64 解码
    │   4. params.session.path → Base64 解码
    │   │
    │   什么是 Base64 解码？
    │   ─────────────────
    │   Base64 是一种把二进制数据编码成纯文本的方法。
    │   比如把 "Hello" 编码成 "SGVsbG8="。
    │   VS Code 某些版本把 sessionId 用 Base64 编码后藏在路径里
    │   Git AI 需要把它解码回来才能拿到真正的 sessionId。
    │   │
    │   如果都拿不到 → 跳过（"我不知道这是哪个 Copilot 会话的编辑"）
    │   │
    │   什么是 sessionId？
    │   ─────────────────
    │   Copilot Chat 的每次对话有一个唯一的 UUID
    │   例如：01f62e6b-9812-4964-b9a6-c4fd0ce15fa2
    │   这个 ID 关联到 Copilot 在本地保存的对话记录文件
    │
    ├─ 【条件 D：必须能定位到 Copilot 的会话文件】
    │   chatSessionsDir = VS Code storage 路径 / chatSessions
    │   尝试找到以下文件之一：
    │   • chatSessionsDir / {sessionId}.jsonl
    │   • chatSessionsDir / {sessionId}.json
    │   │
    │   什么是会话文件？
    │   ─────────────────
    │   VS Code 会把 Copilot Chat 的完整对话记录保存在本地
    │   .jsonl 格式（每行一个 JSON 对象）或 .json 格式
    │   内容包括：用户输入、Copilot 回复、工具调用、编辑操作
    │   Git AI 需要这个文件来提取：用了哪个模型、对话内容是什么
    │
    ├─ 【条件 E：文件必须属于某个工作区】
    │   调用 vscode.workspace.getWorkspaceFolder(...)
    │   如果文件不属于任何工作区 → 跳过
    │
    └─ 全部条件通过 → 发送 AI checkpoint：
       │
       ├─ 收集 dirty_files（所有未保存文件的内容）
       │
       ├─ 强制把刚保存的文件内容也加进去
       │   内容取自 VS Code 编辑器内存（不是从磁盘读）
       │   │
       │   为什么取编辑器内存而不是磁盘？
       │   ──────────────────────────────
       │   在远程开发（如 GitHub Codespaces）中
       │   编辑器保存后磁盘可能有延迟
       │   从编辑器内存读能保证拿到的是真正刚保存的内容
       │
       └─ 调用 checkpoint("ai", hookInput)
          其中 hookInput = {
            hook_event_name: "after_edit",
            chat_session_path: 会话文件路径,
            session_id: Copilot 会话 UUID,
            edited_filepaths: [被改的文件路径],
            workspace_folder: 工作区路径,
            dirty_files: { 文件路径: 文件内容, ... }
          }
```

#### 1.4 checkpoint() 函数——怎么把数据传给 Rust

```
checkpoint(author, hookInput)
    │
    ├─ 如果不是 ai_tab 且旧方案已禁用 → 跳过（避免和新方案重复）
    │
    ├─ 检查 git-ai CLI 是否安装且版本 ≥ 1.0.23
    │
    ├─ 确定 workspaceRoot：
    │   优先用 git 仓库根目录，回退到工作区文件夹
    │
    ├─ 组装命令行参数：
    │   如果 author 是 "ai_tab" → ["checkpoint", "ai_tab", "--hook-input", "stdin"]
    │   否则                    → ["checkpoint", "github-copilot", "--hook-input", "stdin"]
    │
    ├─ spawn 子进程：git-ai checkpoint github-copilot --hook-input stdin
    │   工作目录 = workspaceRoot
    │
    └─ 把 hookInput（JSON 字符串）写入子进程的 stdin
```

> **什么是 spawn 子进程？**
>
> "spawn"在编程里的意思是"启动一个新的程序"。
> "子进程"就是由当前程序启动的另一个程序。
> 这里 VS Code 扩展（父进程）启动了 `git-ai`（子进程）。
> 它们是两个独立运行的程序，通过 stdin 管道传数据。
>
> 打个比方：你（VS Code 扩展）写了一张纸条，
> 塞进另一个房间的门缝下（stdin），
> 房间里的人（git-ai）捡起来看，根据内容做事。

> **关键点**：扩展不会直接调用 Rust 内部函数。
> 它是启动一个独立的 `git-ai` 命令行进程，通过 stdin 传数据。
> 这意味着即使扩展崩溃，Rust 侧已经收到的数据也不会丢失。

#### 1.5 旧方案的防误判措施汇总

| 措施 | 参数 | 作用 |
|------|------|------|
| 快照 scheme 检查 | `chat-editing-snapshot-text-model` | 只有 Copilot Chat 编辑会产生这种文档，普通编辑不会 |
| 快照年龄上限 | 10 秒 | 过了 10 秒的旧快照不再认为有效 |
| sessionId 解析 | 3 种格式兼容 | 必须和一个具体的 Copilot 会话绑定 |
| 会话文件定位 | .jsonl 或 .json | 本地必须有这个会话的对话记录 |
| 工作区检查 | workspaceFolder | 文件必须在项目目录里 |
| 人工 checkpoint 防抖 | 500 毫秒 | 同一文件 500ms 内不重复打 before_edit |
| 保存事件防抖 | 300 毫秒 | 连续保存只处理最后一次 |
| 稳定内容缓存 | 2 秒静默期 | 确保 before_edit 拿到的是 AI 动手前的干净内容 |

---

### 通道二：Copilot Chat 编辑（新方案 Native Hooks）

从 VS Code 1.109.3 开始，VS Code 原生支持在 Copilot 工具调用前后触发钩子，不需要扩展自己监听快照文档了。

#### 2.1 钩子怎么安装——从头解释

##### 先搞懂"钩子"到底是什么

想象你家门口有个包裹柜，你可以设定一条规则："每次有快递投入包裹柜时，自动拍一张照片通知我。"

在软件世界里，"钩子（hook）"就是这种规则：**你当提前注册一条指令，告诉某个程序"某个事情发生时，请自动执行我给你的命令"。**

在这里：
- "某个程序" = VS Code（代码编辑器）
- "某个事情" = Copilot 准备使用工具（如编辑文件、创建文件）
- "我给你的命令" = `git-ai checkpoint github-copilot --hook-input stdin`

所以当 Copilot 在 VS Code 里要编辑文件时，VS Code 会自动执行 git-ai 的命令，把编辑相关的信息传给它。

##### VS Code 具体怎么知道有这个钩子？

VS Code 自 1.109.3 版本开始，内建了一套叫做 **Chat Hooks（聊天钩子）** 的机制。它的工作方式是：

1. VS Code 会去查看一个特定的**配置目录**，看看里面有没有钩子配置文件
2. 如果有，就按照配置文件里写的规则，在合适的时机触发命令

这个配置目录的路径是你电脑上的：

```
~/.copilot/hooks/
```

> `~` 代表你的用户主目录。
> 在 Windows 上通常是 `C:\Users\你的用户名\`。
> 在 Mac/Linux 上通常是 `/home/你的用户名/` 或 `/Users/你的用户名/`。
>
> 所以完整路径在 Windows 上是：`C:\Users\你的用户名\.copilot\hooks\`
>
> 历史路径提醒：Git AI 早期版本使用的是 `~/.github/hooks/git-ai.json`。
> 现在这是**遗留（legacy）路径**，源码里仍会识别它作为“已安装”，
> 但一旦你重新运行 `git-ai install-hooks`，旧路径下的文件会被自动删除，
> 仅保留新路径 `~/.copilot/hooks/git-ai.json`。

Git AI 会在这个目录下创建一个文件叫 `git-ai.json`，完整路径是：

```
~/.copilot/hooks/git-ai.json
```

##### 这个配置文件长什么样？

当你执行 `git-ai install-hooks` 命令后，上面说的 `git-ai.json` 文件会被自动创建，内容大致是这样的：

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "/usr/local/bin/git-ai checkpoint github-copilot --hook-input stdin"
      }
    ],
    "PostToolUse": [
      {
        "type": "command",
        "command": "/usr/local/bin/git-ai checkpoint github-copilot --hook-input stdin"
      }
    ]
  }
}
```

> `/usr/local/bin/git-ai` 是 git-ai 程序在你电脑上的安装位置，
> 不同的电脑上路径不同，安装时会自动填入你的实际路径。

逐行解释：

| JSON 键 | 含义 |
|---------|------|
| `hooks` | 顶层容器，里面放所有钩子规则 |
| `PreToolUse` | "在工具使用之**前**触发"。这里的"工具"指 Copilot Chat 在工作时调用的各种功能（编辑文件、创建文件等）。对应 Copilot 要动手**之前**。 |
| `PostToolUse` | "在工具使用之**后**触发"。对应 Copilot 动手**之后**。 |
| `type: "command"` | 告诉 VS Code：这条钩子的类型是"执行一个命令行命令" |
| `command: "...git-ai checkpoint..."` | 要执行的具体命令。`--hook-input stdin` 意思是"我通过标准输入接收数据" |

**通俗总结**：这个文件就像是一张"告示单"，贴在 VS Code 能看到的地方，上面写着：

> "每当 Copilot 要用工具的时候（PreToolUse），请执行 `git-ai checkpoint ...` 命令。
> 每当 Copilot 用完工具的时候（PostToolUse），请再执行一次同样的命令。"

##### VS Code 怎么知道去读这个文件？

从 VS Code 近期版本开始，**Chat Hooks 机制会自动扫描一组约定目录**（包括 `~/.copilot/hooks/`），
只要里面有 `git-ai.json`，就会加载。你不需要再在 settings.json 里手动告诉 VS Code
去哪里找钩子配置。

Git AI 的安装过程（`git-ai install-hooks`）只会往 VS Code settings.json 里加一行总开关：

```json
{
  "chat.useHooks": true
}
```

解释：

| 设置项 | 含义 |
|--------|------|
| `chat.useHooks` | 总开关：“是否启用聊天钩子功能”。设为 `true` = 启用 |

> **历史提醒**：早期版本还会写入 `chat.hookFilesLocations`，
> 告诉 VS Code “去 `~/.github/hooks/` 找钩子”。
> 现在不再写入这个字段——路径走的是 VS Code 原生约定的 `~/.copilot/hooks/`，
> 只需要 `chat.useHooks` 一个总开关就够了。

##### 安装过程的完整流程（把上面串起来）

```
用户执行 git-ai install-hooks
    │
    ├─ 第1步：创建钩子配置文件
    │   位置：~/.copilot/hooks/git-ai.json
    │   内容：PreToolUse 和 PostToolUse 两个钩子
    │         每个钩子的命令 = git-ai 二进制文件的路径 + checkpoint 参数
    │   如果检测到旧路径 ~/.github/hooks/git-ai.json 存在，会被自动删除
    │
    ├─ 第2步：修改 VS Code settings.json
    │   添加 "chat.useHooks": true（如已存在但值为 false，会被覆盖为 true）
    │   （不再需要 chat.hookFilesLocations：~/.copilot/hooks/ 是约定路径）
    │
    └─ 完成。从此之后：
       每次 Copilot 调用工具前 → VS Code 自动执行 git-ai checkpoint（PreToolUse）
       每次 Copilot 调用工具后 → VS Code 再次自动执行 git-ai checkpoint（PostToolUse）
```

##### 核心问题：VS Code 怎么知道 Copilot 要做事？

这是整个新方案里最关键的一个问题。要回答它，必须先理解 **Copilot 在 VS Code 里到底是怎么工作的**。很多人以为 Copilot 是直接操作你的文件——其实不是。

###### Copilot 不能直接碰你的文件

**Copilot 本身只是一个"大脑"，它不能直接动手。**

你在 Copilot Chat 里说"帮我写一个排序函数"，会发生什么？

1. 你的消息被发送到远程的 AI 大模型（比如 GPT-4o、Claude Sonnet 等）
2. AI 大模型"思考"后，**不是**直接往你的文件里写代码
3. AI 大模型返回的是一条**指令**，类似于："请调用 `editFile` 工具，参数是：文件路径 = `src/main.rs`，新内容 = `fn sort(...) {...}`"

**关键点：AI 大模型只能"说话"（返回文字），不能"动手"（直接操作文件）。**

这就像你去餐厅点菜——你只是告诉服务员"我要一份牛排"，而不是自己跑进厨房做。AI 大模型就是那个"点菜的人"，VS Code 则是"服务员 + 厨师"。

###### 什么是"工具调用"（Tool Calling）？

上面说的"AI 返回一条请求调用某工具的指令"，在技术上叫做 **Tool Calling（工具调用）**。

这是现代 AI 大模型的一个标准功能。工作方式是：

1. **注册工具**：告诉 AI "你有这些工具可以用"，每个工具有名字、描述、参数格式
2. **AI 做决策**：AI 根据用户的要求，决定"我需要用哪个工具、传什么参数"
3. **AI 输出调用请求**：AI 不是直接执行，而是输出一个结构化的"工具调用请求"
4. **宿主程序执行**：VS Code（宿主程序）收到这个请求后，**真正执行**对应的操作

打个比方：

> AI 就像一个只能写纸条的指挥官。
> 它写纸条说："请把 `src/main.rs` 文件的第 10 行改成 `let x = 42;`"
> VS Code 是拿到纸条后**真正去改文件**的执行者。
> AI 永远不会直接接触你的文件系统。

所以 Copilot 在 VS Code 里的具体流程是：

```
用户在 Copilot Chat 里说："帮我写一个排序函数"
    │
    ▼
VS Code 把你的消息 + 上下文 + 可用工具列表 发送给 AI 大模型
    │
    ▼
AI 大模型返回（不是直接改文件！而是返回一条指令）：
    "我需要调用 editFile 工具"
    "参数：=file_path  src/main.rs"
    "参数：new_content = fn sort(arr: &mut [i32]) { ... }"
    │
    ▼
VS Code 收到这条指令
    │
    ├─ ⭐ 此时 VS Code 完全知道：
    │   • Copilot 想做什么 → 调用 editFile（编辑文件）
    │   • 要操作哪个文件 → src/main.rs
    │   • 要写什么内容 → fn sort(arr: &mut [i32]) { ... }
    │   • 这是哪个对话 → session_id
    │
    ├─ ⭐ 在真正执行之前，VS Code 会先触发 PreToolUse 钩子
    │   → 执行 git-ai checkpoint（拍"编辑前"快照）
    │
    ├─ VS Code 真正执行 editFile 操作（把新代码写进 src/main.rs）
    │
    └─ 执行完成后，VS Code 触发 PostToolUse 钩子
       → 再次执行 git-ai checkpoint（拍"编辑后"快照）
```

**所以核心答案是：VS Code 之所以能"知道" Copilot 要做事，是因为 Copilot 根本不能自己做事——一切操作都必须经过 VS Code 来执行。VS Code 是唯一的执行者，它天然知道所有操作的全部细节。**

这不是"监听"或"拦截"——而是 Copilot 的所有操作请求**本来就是发给 VS Code 的**。就像快递公司天然知道每个包裹的收件地址，因为所有包裹都是经它手送的。

###### Copilot 可以调用哪些"工具"？

VS  Code 里给 Copilot注册了很多工具，大致分两类：

**编辑类工具**（会改代码，Git AI 需要追踪）：

| 工具名 | 作用 |
|--------|------|
| `editFile` / `edit` | 编辑一个文件 |
| `create_file` | 创建新文件 |
| `delete_file` | 删除文件 |
| `rename_file` / `move_file` | 重命名或移动文件 |
| `replace_string_in_file` | 替换文件里的某段文字 |
| `insert_edit_into_file` | 在文件的某个位置插入代码 |
| `multiedit` | 同时编辑多个位置 |
| `applypatch` / `apply_patch` | 应用一个补丁（批量修改），git-ai 能从补丁文本里解析出 `*** Update File:` / `*** Add File:` / `*** Delete File:` / `*** Move to:` 行，提取被影响的文件路径 |
| `copilot_insertedit` | Copilot 专用的插入编辑 |
| `copilot_replaceString` | Copilot 专用的文字替换 |

**Bash / 终端类工具**（会改磁盘上的文件，但不是直接改编辑器缓冲区，Git AI 走单独的 bash_tool 路径追踪）：

| 工具名 | 作用 |
|--------|------|
| `run_in_terminal` | 在终端里执行命令，可能写文件、删除文件、生成构建产物等 |

**非编辑类工具**（只是读取/搜索，不改代码，Git AI 会忽略）：

| 工具名 | 作用 |
|--------|------|
| `readFile` / `read` | 读取文件内容 |
| `findTextInFiles` / `search` / `grep` | 搜索文本 |
| `listFiles` / `ls` / `glob` | 列出文件 |
| `fetchWebpage` / `web` | 获取网页内容 |

**为什么要区分？**

Copilot 在工作时会频繁调用各种工具——先搜索代码了解结构，然后再编辑。如果不区分，"Copilot 搜索了一下代码"也会被 Git AI 误判成"Copilot 修改了代码"。所以 Git AI 只关心编辑类工具的调用，非编辑类工具直接跳过。

###### "工具调用请求"长什么样？——AI 大模型的实际输出

当 AI 大模型决定要编辑文件时，它实际输出的内容类似这样（简化版）：

```json
{
  "tool_calls": [
    {
      "id": "call_abc123",
      "type": "function",
      "function": {
        "name": "editFile",
        "arguments": "{\"file_path\": \"src/main.rs\", \"new_content\": \"fn sort() { ... }\"}"
      }
    }
  ]
}
```

> 这段 JSON 不是 Copilot 写给你的回复——你看不到它。
> 这是 AI 大模型返回给 VS Code 的**"内部指令"**。
> VS Code 解析这个指令后知道：要调用 `editFile`，参数是文件路径和新内容。

VS Code 解析完这个指令后，会：
1. 提取出 `name`（工具名）→ `editFile`
2. 提取出 `arguments`（参数）→ `{ file_path: "src/main.rs", new_content: "..." }`
3. 在执行之前，触发 PreToolUse 钩子
4. 执行工具
5. 执s行之后，触发 PostToolUe 钩子

##### 触发时 VS Code 传了什么数据？

现在你已经知道了 VS Code 为什么能知道 Copilot 的一切——因为所有工具调用都是经 VS Code 手执行的。接下来看 VS Code 在触发钩子时，具体传了什么数据给 git-ai。

每次钩子被触发时，VS Code 会把些上面说的工具调用信息，加上一额外的上下文，打包成一段 JSON 数据，通过 stdin 传给 git-ai 命令。

**PreToolUse（工具执行前）传的数据：**

```json
{
  "hookEventName": "PreToolUse",
  "cwd": "/Users/test/project",
  "toolName": "copilot_replaceString",
  "toolInput": {
    "file_path": "src/main.ts"
  },
  "transcript_path": "/Users/test/Library/Application Support/Code/User/workspaceStorage/workspace-id/GitHub.copilot-chat/transcripts/copilot-session-pre.jsonl",
  "sessionId": "copilot-session-pre"
}
```

> 注意：以上是从项目测试代码 `tests/integration/github_copilot.rs` 里提取的**真实测试数据**，不是我编的。

**PostToolUse（工具执行后）传的数据：**

```json
{
  "hookEventName": "PostToolUse",
  "cwd": "/Users/test/project",
  "toolName": "copilot_replaceString",
  "toolInput": {
    "file_path": "/Users/test/project/src/main.ts"
  },
  "sessionId": "copilot-session-post",
  "transcript_path": "/Users/test/.../GitHub.copilot-chat/transcripts/copilot-session-post.jsonl"
}
```

**再看一个创建文件的例子（同样来自真实测试数据）：**

```json
{
  "hookEventName": "PreToolUse",
  "cwd": "/Users/test/project",
  "toolName": "create_file",
  "toolInput": {
    "filePath": "/Users/test/project/src/new-file.ts",
    "content": "export const x = 1;\n"
  },
  "transcript_path": "...",
  "sessionId": "copilot-session-create"
}
```

**批量编辑多个文件的例子：**

```json
{
  "hookEventName": "PreToolUse",
  "cwd": "/Users/test/project",
  "toolName": "editFiles",
  "toolInput": {
    "files": ["src/main.ts", "/Users/test/project/src/other.ts"]
  },
  "transcript_path": "...",
  "sessionId": "copilot-session-editfiles"
}
```

逐字段逐个解释：

| 字段 | 含义 | 怎么来的 |
|------|------|---------|
| `hookEventName` | `"PreToolUse"` 或 `"PostToolUse"` —— 这次是工具执行前还是执行后 | VS Code 自动填写 |
| `cwd` | 当前工作目录（你的项目文件夹路径） | VS Code 自动填写 |
| `toolName` | Copilot 要调用哪个工具，比如 `"copilot_replaceString"`、`"create_file"`、`"editFiles"` | 来自 AI 大模型的工具调用指令 |
| `toolInput` | 工具的参数。不同工具的参数不同：编辑文件会有 `file_path` 和内容，创建文件会有 `filePath` 和 `content`，批量编辑会有 `files` 数组 | 来自 AI 大模型的工具调用指令 |
| `transcript_path` | Copilot 对话记录文件在你电脑上的位置 | VS Code 自动填写 |
| `sessionId` | 这次对话的唯一标识（UUID） | VS Code 自动填写 |

> **"来自 AI 大模型"和"VS Code 自动填写"有什么区别？**
>
> `toolName` 和 `toolInput` 这两个字段的内容来自 AI 大模型的"工具调用指令"——
> 是 AI 决定要用什么工具、传什么参数。
>
> 而 `hookEventName`、`cwd`、`transcript_path`、`sessionId` 这些是 VS Code 自己加上去的——
> AI 大模型不知道你的项目在哪个目录、对话记录存在哪个文件里，这些是 VS Code 的上下文信息。
>
> VS Code 把"AI 的指令"+"自己的上下文"合并成一个完整的 JSON 包，传给 git-ai。

###### 完整的时序图：从你说话到 git-ai 收到数据

```
时间线 ──────────────────────────────────────────────────────────►

1. 你在 Copilot Chat 里输入："帮我在 main.rs 里写一个排序函数"
   │
   ▼
2. VS Code 把你的消息 + 聊天历史 + 可用工具列表  
   发送到 GitHub 的 AI 服务器
   │
   ▼
3. AI 服务器上的大模型思考后返回：
   "请调用 editFile 工具，参数 = { file_path: 'src/main.rs', new_content: '...' }"
   （注意：AI 只返回了一条文字指令，没有碰你的文件）
   │
   ▼
4. VS Code 收到 AI 的回复，解析出工具调用请求：
   • 工具名 = editFile
   • 参数 = { file_path: "src/main.rs", new_content: "fn sort..." }
   │
   ▼
5. VS Code 准备执行 editFile，但在执行前：
   ┌─ 检查 settings.json 里 chat.useHooks 是否为 true ✓
   ├─ 扫描内置约定的钩子目录（包含 ~/.copilot/hooks/）✓
   ├─ 读取 ~/.copilot/hooks/git-ai.json ✓
   │  （旧路径 ~/.github/hooks/git-ai.json 仅作为遗留路径被识别）
   ├─ 发现 PreToolUse 钩子 → 需要先执行它
   │
   ├─ 组装 JSON 数据包：
   │   {
   │     hookEventName: "PreToolUse",
   │     toolName: "editFile",
   │     toolInput: { file_path: "src/main.rs", new_content: "..." },
   │     cwd: "/your/project",
   │     sessionId: "xxx-xxx-xxx",
   │     transcript_path: "/path/to/session.jsonl"
   │   }
   │
   └─ 启动命令：git-ai checkpoint github-copilot --hook-input stdin
      把 JSON 数据包通过 stdin 灌进去
   │
   ▼
6. git-ai 收到 PreToolUse 数据 → 拍"编辑前"快照（Human checkpoint）
   │
   ▼
7. VS Code 真正执行 editFile：
   把 "fn sort..." 写入 src/main.rs
   │
   ▼
8. 编辑完成。VS Code 再次触发钩子：
   ┌─ 组装 PostToolUse JSON 数据包（和第5步类似，但 hookEventName = "PostToolUse"）
   └─ 再次启动 git-ai checkpoint，灌入 JSON
   │
   ▼
9. git-ai 收到 PostToolUse 数据 → 拍"编辑后"快照（AiAgent checkpoint）
   → 和"编辑前"快照做 diff → 计算出哪些字符是 AI 写的
```

**一句话总结：VS Code 不需要"检测"Copilot 在做事——因为 Copilot 的所有操作指令都是发给 VS Code 的，VS Code 是唯一的执行者，它天然拥有全部信息。钩子机制只是让 VS Code 在执行前后通知 git-ai 一声。**

##### 为什么这种方式比旧方案好？

| 对比项 | 旧方案（Legacy Hooks） | 新方案（Native Hooks） |
|--------|----------------------|----------------------|
| 谁负责检测 | Git AI 的 VS Code 扩展 自己监听编辑器事件 | VS Code **原生内建**的钩子机制 |
| 可靠性 | 依赖间接信号（快照文档 scheme），有时效窗口（10秒） | 直接拦截工具调用，不会遗漏 |
| 信息量 | 需要自己拼凑 sessionId、会话路径 | VS Code 直接把全部信息打包好传过来 |
| 精确度 | 可能误判（快照文档打开≠一定会编辑） | 精确到每次工具调用 |
| 兼容性 | 所有 VS Code 版本 | 需要 VS Code ≥ 1.109.3 |

#### 2.2 事件到达 Rust 后的处理

所有处理逻辑在 `GithubCopilotPreset::run_vscode_native_hooks()` 里，地址是 `src/commands/checkpoint_agent/agent_presets.rs`。

```
收到 PreToolUse 或 PostToolUse 的 JSON
    │
    ├─ 【第 1 层过滤：工具名检查】
    │   调用 is_supported_vscode_edit_tool_name(tool_name)
    │   │
    │   特例放行（精确匹配）：
    │   run_in_terminal  ← 走 bash_tool 快照链路，不是普通 file edit
    │   │
    │   拒绝列表（包含以下关键词的工具名直接跳过）：
    │   find, search, read, grep, glob, list, ls,
    │   fetch, web, open, todo
    │   │
    │   接受列表（精确匹配以下名称直接放行）：
    │   write, edit, multiedit, applypatch, apply_patch,
    │   copilot_insertedit, copilot_replacestring,
    │   vscode_editfile_internal, create_file,
    │   delete_file, rename_file, move_file,
    │   replace_string_in_file, insert_edit_into_file
    │   │
    │   模糊接受（包含以下关键词的工具名放行）：
    │   edit, write, replace
    │   │
    │   为什么要过滤？
    │   ────────────────
    │   Copilot 不是只编辑文件，它还会搜索文件、读文件、运行命令
    │   这些操作不改代码，如果不过滤
    │   "Copilot 搜索了一下代码"也会被当成"Copilot 改了代码"
    │   所以只有编辑类工具才需要做归因
    │   │
    │   不满足 → 返回错误，跳过这次 hook
    │
    ├─ 【第 2 层过滤：提取文件路径——严格只看本次工具调用】
    │   调用 extract_filepaths_from_vscode_hook_payload()
    │   ⚠️ 只从本次 hook 的 tool_input 和 tool_response 中提取路径
    │   绝不合并 hook_data.edited_filepaths / will_edit_filepaths 等
    │   会话级字段
    │   │
    │   为什么要这样严格？
    │   ──────────────────
    │   Copilot 在一个会话里常常连续调用多个工具
    │   会话级字段可能包含之前工具调用留下的陈旧文件列表
    │   如果合并进来，就会出现"这次只改了 A，却把 B 也当成本次编辑"
    │   的交叉污染，尤其在连续多文件操作时非常明显
    │   │
    │   从 tool_input 和 tool_response 的 JSON 里递归查找文件路径：
    │   │
    │   什么是"递归查找"？
    │   ──────────────────
    │   JSON 可以嵌套很多层，比如 { "a": { "b": { "path": "xxx" } } }
    │   "递归"就是一层一层往下钻，直到找到目标字段为止
    │   不管文件路径藏在 JSON 的哪一层，都能找到
    │   │
    │   查找以下字段名：
    │   • 单文件键名：file_path, filepath, path, fspath
    │   • 多文件键名：files, filepaths, file_paths
    │   • 以 file:// 开头的字符串
    │   │
    │   所有路径会被规范化为绝对路径
    │   │
    │   什么是"绝对路径"？
    │   ──────────────────
    │   完整的文件路径，从根目录开始写，不省略任何部分
    │   比如 "C:\Users\你\project\src\main.rs" 是绝对路径
    │   而 "src\main.rs" 是相对路径（省略了前面的部分）
    │   规范化为绝对路径确保系统能准确定位文件
    │   │
    │   如果一个文件路径都找不到（且是 PreToolUse）→ 跳过
    │
    ├─ 【第 3 层过滤：确认是 Copilot 的会话】
    │   从 hook_data 里找 transcript_path / chat_session_path
    │   │
    │   调用 looks_like_copilot_transcript_path(path)
    │   路径必须包含以下之一：
    │   • /github.copilot-chat/transcripts/
    │   • vscode-chat-session
    │   • copilot_session
    │   • /workspacestorage/ 且含 /chatsessions/
    │   │
    │   如果路径像 Claude 的 → 明确排除
    │     • 路径包含 /.claude/
    │     • 路径包含 /claude/projects/
    │   如果路径不像 Copilot → 明确排除
    │   │
    │   为什么要排除 Claude？
    │   ──────────────────
    │   Claude Code 也支持在 VS Code 里运行
    │   它的 hook 格式和 Copilot 很像
    │   但 Git AI 有专门的 Claude preset 处理它
    │   这里必须排除，避免一次编辑被两个 preset 同时处理
    │
    ├─ 【第 4 步：按工具分类（classify_copilot_tool）】
    │   ToolClass::FileEdit → 普通改文件路径
    │   ToolClass::Bash     → 走 bash_tool 快照链路（见 2.4）
    │   ToolClass::Skip     → 忽略
    │
    ├─ 如果是 PreToolUse：
    │   • FileEdit 工具：
    │       返回 checkpoint_kind = Human
    │       will_edit_filepaths = 提取到的文件路径
    │       （和旧方案一样，这是在记录"AI 动手前的人工基线"）
    │
    │   • 特例：create_file 的 PreToolUse
    │       此时文件还不存在。git-ai 不去读磁盘（否则可能读到并发
    │       工具调用留下的临时内容），而是合成 dirty_files =
    │       { file_path: "" }，明确表示"文件此刻还是空的"。
    │
    │   • Bash 工具（run_in_terminal）：
    │       PreToolUse 时不生成 Human checkpoint，只为本次 tool_use_id
    │       拍一张仓库 stat 快照（见 2.4）。
    │
    └─ 如果是 PostToolUse：
       • FileEdit 工具：
         返回 checkpoint_kind = AiAgent
         edited_filepaths = 提取到的文件路径
         agent_id = {
           tool: "github-copilot",
           id: session_id,
           model: 从会话文件解析出的模型名（如 "copilot/claude-sonnet-4"）
         }
         transcript = 从会话文件解析出的对话记录

       • Bash 工具（run_in_terminal）：
         对比 PreToolUse 时的仓库快照和当前磁盘状态，
         得到真正被终端命令改动的文件列表，再按这个列表
         生成 AiAgent checkpoint（见 2.4）。
```

#### 2.3 模型信息怎么获取

Rust 会读取 Copilot 的会话文件（`.jsonl` 或 `.json`），解析出：

- 用户消息
- Copilot 回复
- 工具调用（包括 `textEditGroup` 里编辑了哪些文件）
- **模型 ID**（例如 `copilot/claude-sonnet-4`、`copilot/gpt-4o`）

如果从 transcript 里拿到的模型是空、`unknown` 或 `copilot/auto`，
git-ai 会继续走一次 fallback：

1. 从 transcript 路径 `.../github.copilot-chat/transcripts/xxx.jsonl` 推出
   同级的 `chatSessions/` 目录
2. 扫描该目录下所有 `.json` / `.jsonl` 文件，找包含当前 session_id 的文件
3. 优先提取以下候选，取第一个非 `copilot/auto` 的值：
   - 该 session 里某个 `request.result.metadata.sessionId == 当前 session_id`
     的 `request.modelId`
   - `session.inputState.selectedModel.identifier`
4. 如果全都没命中非 auto 的值，才退回 `copilot/auto` 或 `unknown`

这个 fallback 解决的是一个实际问题：**有些 Copilot 会话在 transcript
里只写了 `copilot/auto`，真实使用的具体模型（Claude Sonnet、GPT 等）
只在同级的 `chatSessions/` 里记录。**

#### 2.4 Bash 工具（run_in_terminal）怎么归因

`run_in_terminal` 和普通编辑工具不一样：它执行的是一整条 shell 命令，
工具参数里没有确切的 file_path，但命令执行过程可能改动、创建、删除文件。
如果不专门处理，这类改动就会被漏记成"凭空出现的未归因代码"。

Rust 侧的处理链路（`src/commands/checkpoint_agent/bash_tool.rs`）：

```
PreToolUse (run_in_terminal)
    │
    ├─ 不生成 Human checkpoint
    │
    └─ 对仓库做一次 stat 快照
       • 扫描工作区里所有未被 .gitignore / .git-ai-ignore 忽略的文件
       • 对每个文件记录：是否存在、大小、mtime、inode 等指纹
       • 快照以 tool_use_id 为 key 存入临时区
       • 受硬性限额保护：
           WALK_TIMEOUT_MS ≈ 1500ms    单次扫描超时
           HOOK_TIMEOUT_MS ≈ 4000ms    整个 hook 超时
           MAX_TRACKED_FILES = 50_000   超过直接 fallback
           SNAPSHOT_STALE_SECS = 300   过期快照回收

PostToolUse (run_in_terminal)
    │
    ├─ 按 tool_use_id 取回 PreToolUse 时的 stat 快照
    │
    ├─ 再扫一次当前仓库，得到"之后"的 stat 快照
    │
    ├─ 对比两份快照，按 mtime、size、inode 差异挑出真正变动的文件
    │   • 太多"陈旧文件"（> MAX_STALE_FILES_FOR_CAPTURE = 1000）
    │     → 放弃内容捕获，走 fallback
    │   • 单文件超过 MAX_CAPTURE_FILE_SIZE (10 MB)
    │     → 跳过这个文件的内容捕获
    │
    └─ 最终产出 BashCheckpointAction：
       • Checkpoint(paths)  → 按 paths 生成 AiAgent checkpoint
       • NoChanges          → 命令执行了但没改文件，不生成 checkpoint
       • Fallback           → 超时 / 仓库太大，放弃本次归因
       • TakePreSnapshot    → 防御式状态，正常不会出现在 PostToolUse
```

和 FileEdit 工具的核心区别：

- **FileEdit**：Copilot 自己告诉你它要改哪个文件 → 从 tool_input 直接取路径
- **Bash**：Copilot 只给你一条命令 → git-ai 自己对比仓库前后状态反推文件列表

这条链路还有一个重要的"安全阀"：任何一步超出时间或体积限额，
都会返回 `Fallback` 让 checkpoint 被跳过，而不是阻塞你的编辑流程。

---

### 通道三：Copilot Tab 补全

**Tab 补全和 Chat 编辑完全不同**。Chat 编辑是 Copilot 主动改文件，Tab 补全是 Copilot 提供建议、用户按 Tab 接受。

#### 3.1 它怎么拦截 Tab 操作

在 `ai-tab-edit-manager.ts` 里：

```
扩展启动时
    │
    └─ registerCommand() 劫持 Tab 接受命令
       │
       │  不同 IDE 的命令名不同：
       │  • VS Code: "editor.action.inlineSuggest.commit"
       │  • Cursor:  "editor.action.acceptCursorTabSuggestion"
       │
       │  劫持的意思是：
       │  扩展用自己的函数替换了原始的命令处理器
       │  用户按 Tab 时，先经过扩展的代码，再调用原始处理器
       │
       └─ 每次用户按 Tab 接受补全时：
          │
          ├─ 第一步：beforeHook()
          │   缓存所有打开文件的当前内容
          │   存入 beforeCompletionFileStates
          │
          ├─ 第二步：调用原始命令
          │   Copilot 的补全内容被插入编辑器
          │
          └─ 第三步：afterHook()
             ├─ 取 beforeContent（缓存的旧内容）
             ├─ 取 afterContent（补全后的新内容）
             │
             ├─ 发送 before_edit（带旧内容）
             │   hook_event_name: "before_edit"
             │   tool: "github-copilot-tab"
             │   model: "default"
             │   will_edit_filepaths: [文件路径]
             │   dirty_files: { 文件路径: 旧内容 }
             │
             └─ 发送 after_edit（带新内容）
                hook_event_name: "after_edit"
                tool: "github-copilot-tab"
                model: "default"
                edited_filepaths: [文件路径]
                dirty_files: { 文件路径: 新内容 }
```

注意：Tab 补全走的 preset 是 `ai_tab`，不是 `github-copilot`。Rust 侧会把它记为 `CheckpointKind::AiTab`，和 Chat 编辑的 `AiAgent` 区分开。

#### 3.2 Tab 补全 vs Chat 编辑的区别

| 对比项 | Chat 编辑 | Tab 补全 |
|--------|----------|---------|
| 触发方式 | Copilot 主动改文件 | 用户按 Tab 接受建议 |
| 检测方式 | 监听快照文档 / 原生 hook | 劫持 Tab 命令 |
| CheckpointKind | `AiAgent` | `AiTab` |
| CLI preset | `github-copilot` | `ai_tab` |
| 涉及文件数 | 可以同时改多个 | 通常只改当前文件 |
| 会话信息 | 有 sessionId、对话记录、模型信息 | 只知道 tool 和 model="default" |
| 实验状态 | 正式功能 | 实验功能，需手动开启 |

---

### 通道四：KnownHuman——人类主动保存检查点

前面三条通道都和 Copilot 有关。还有一条**完全不依赖 Copilot 的检测路径**：每当用户在 VS Code 里保存任意文件时，扩展会主动告诉 Rust：“这次保存是真人按 Ctrl+S 触发的”。

实现位于 `agent-support/vscode/src/known-human-checkpoint-manager.ts`。

#### 4.1 它解决什么问题？

如果只靠 Copilot hook + git pre-commit，会出现下面这种空白：

- AI 编辑后人类继续手工改了几行
- 但人类一直没 commit（比如先去开会了）
- 仅靠 pre-commit hook 看不到这次手工编辑过程，只能看到一个“最终态”

`KnownHuman` 检查点的作用是：**在 commit 之前，就把每一次人类保存动作单独标记出来**，让 Rust 能像区分 AI 编辑那样，区分人类的多次中间编辑，而不是把它们和 AI 编辑搅在一起。

#### 4.2 工作流程

```
用户在 VS Code 中按 Ctrl+S 保存任意文件
    │
    ├─ 扩展的 onDidSaveTextDocument 监听器触发
    ├─ 跳过非 file:// scheme（比如未保存的 untitled）
    ├─ 跳过路径里包含 /.vscode/ 的内部文件
    ├─ 通过 getGitRepoRoot() 找到该文件所在仓库根目录
    │   找不到 → 跳过（非 git 仓库不追踪）
    │
    ├─ 把这次保存的绝对路径加入“repo root → pending Set”队列
    │
    └─ 防抖 500ms：
        │   500ms 内同一仓库的所有保存合并成一次 checkpoint
        │   （“Save All” 一次性保存多个文件时也只触发一次）
        │
        └─ 时间到 → executeCheckpoint(repoRoot)
            │
            ├─ 收集 dirty_files：
            │     • 优先从 VS Code 编辑器内存读取（能避免远程开发场景的磁盘延迟）
            │     • 文档已关闭则 fallback 读取磁盘
            │
            └─ spawn: git-ai checkpoint known_human --hook-input stdin
                stdin = {
                  editor: "vscode",
                  editor_version: "1.x.x",
                  extension_version: "0.x.x",
                  cwd: <repo root>,
                  edited_filepaths: [...],
                  dirty_files: { 路径: 当前内容, ... }
                }
```

注意：和旧方案的 Chat 编辑监听**完全独立**——`KnownHumanCheckpointManager` 在 `extension.ts` 启动阶段被无条件注册，无论 VS Code 版本是用 Native Hooks 还是 Legacy Hooks，都会运行。

#### 4.3 Rust 侧怎么处理？

进入 `git-ai checkpoint known_human` 后，Rust 会：

1. 解析 hook 输入里的 `editor` / `editor_version` / `extension_version`，封装成 `KnownHumanMetadata`
2. 创建 `CheckpointKind::KnownHuman` 的 checkpoint，挂上一个固定的 `agent_id = { tool: "known_human", id: "known_human_session", model: "" }`，并把 `KnownHumanMetadata` 写入 checkpoint
3. 走和普通 Human checkpoint 类似的归因更新流程，但**注意：`KnownHuman.is_ai()` 返回 false**——它在归因聚合时仍然算“人类”，只是带上了“何时何处由哪台 IDE 保存”的元数据

#### 4.4 一个重要的“静默期”保护

`src/commands/checkpoint.rs` 里有一段非测试代码专门处理 KnownHuman 的“误报”：

```
KNOWN_HUMAN_MIN_SECS_AFTER_AI = 1 秒

如果本次 KnownHuman 出现的 1 秒内：
  曾在同一个文件上有过任何 AI checkpoint
  → 直接拒绝这次 KnownHuman
```

为什么需要这道闸？因为 Copilot 完成编辑写盘后，VS Code 紧接着会触发一次 `onDidSaveTextDocument`，那其实是 AI 编辑产生的副作用，不是人类按下 Ctrl+S。如果不拦截，会出现“AI 改完立刻被覆盖回 human”的错误归因。

#### 4.5 三种“人类”checkpoint 的角色对比

| 名字 | 触发来源 | 时机 | 用途 |
|------|---------|------|------|
| `Human` | git pre-commit hook 在 commit 前自动跑 | commit 时 | 最后兜底：合算人类对 AI 代码的覆盖 |
| `Human` (before_edit, 旧方案) | VS Code 扩展在检测到 Copilot 快照打开时 | AI 动手前 | 给 AI checkpoint 提供干净的“人工基线” |
| `KnownHuman` | VS Code 扩展在用户保存文件时（防抖 500ms） | 编辑过程中实时 | 显式地把人类的中间编辑跟 AI 编辑分开 |

---

## 第二层：Rust 核心——怎么把编辑事件变成归因结果

> **什么是 CLI（命令行界面）？**
>
> CLI 就是通过在"终端"或"命令提示符"里输入文字命令来控制程序的方式。
> `git-ai` 就是一个 CLI 程序。你（或 VS Code 扩展）在命令行里输入
> `git-ai checkpoint github-copilot --hook-input stdin`，
> 它就开始工作。

VS Code 通过 PreToolUse / PostToolUse 钩子传来的 JSON，到达 Rust 后，会经过四步处理。

### 步骤一：事件路由——把 JSON 翻译成统一结构

入口在 `GithubCopilotPreset::run()` 里：

```
收到 JSON payload
    │
    ├─ 解析 hook_event_name 字段
    │
    ├─ "PreToolUse" 或 "PostToolUse"
    │   → 走新方案处理：run_vscode_native_hooks()
    │
    └─ 其他 → 忽略（退出）
```

两条路最终都输出同一个结构 `AgentRunResult`：

> **什么是"结构（struct）"？**
>
> 在编程中，"结构"就是把多个相关的数据打包成一组。
> 类似纸质表格——表格有很多栏（名字、年龄、地址），
> 填好后就是一条完整的记录。
> `AgentRunResult` 就是一张"事件处理结果表"，包含以下栏目：

```
AgentRunResult {
    agent_id: {
        tool:  "github-copilot"     ← 哪个 AI 工具
        id:    "01f62e6b-..."       ← 会话 UUID（唯一标识符）
        model: "copilot/claude-sonnet-4" ← 使用的 AI 模型
    }
    checkpoint_kind: Human / AiAgent / AiTab
    transcript: 对话记录（可选）
    repo_working_dir: 仓库根目录
    edited_filepaths: ["src/main.rs"]     ← after 事件用
    will_edit_filepaths: ["src/main.rs"]  ← before 事件用
    dirty_files: { "src/main.rs": "文件内容..." }
}
```。

**新方案（run_vscode_native_hooks）的路由细节**：

- **PreToolUse**：
  - 过滤非编辑工具（`readFile`、`search` 等直接跳过）
  - 从 `toolInput` 中提取 `will_edit_filepaths`
  - 返回 `checkpoint_kind = Human`
  - agent_id 固定为 `{ tool: "human", id: "human", model: "human" }`

- **PostToolUse**：
  - 从 `toolInput` / `toolResponse` 中提取 `edited_filepaths`
  - 读取 `transcript_path` 指向的会话文件，解析模型名和对话记录
  - 返回 `checkpoint_kind = AiAgent`
  - agent_id 里的 model 从会话文件解析（如 `copilot/claude-sonnet-4`）

---

### 步骤二：确定要处理哪些文件

函数 `explicit_capture_target_paths()` 的逻辑非常直白：

```
如果 checkpoint_kind 是 Human：
    → 使用 will_edit_filepaths（AI 预告即将编辑的文件）
    → 角色标记为 "WillEdit"

如果 checkpoint_kind 是 AiAgent 或 AiTab：
    → 使用 edited_filepaths（AI 实际编辑了的文件）
    → 角色标记为 "Edited"

过滤掉空路径
如果没有任何有效路径 → 返回 None（不处理）
```

然后对每个文件，系统会做一个**快速路径优化**（在 `get_checkpoint_entry_for_file()` 里）：

```
【快速跳过条件】
如果同时满足以下所有条件：
  • 这是 Human checkpoint
  • 这个文件历史上从未被任何 AI 编辑过（has_prior_ai_edits = false）
  • 这个文件没有初始归因数据

→ 直接跳过这个文件，不做任何计算

为什么可以跳？
─────────────
如果一个文件从没被 AI 碰过，也没有历史归因
那它全部是人类代码
记录"全部是人类"没有意义（人类是默认值）
跳过它可以节省大量计算
```

还有一个**内容一致性检查**：

```
如果 当前文件内容 == 上一个 checkpoint 时的内容：
    → 跳过（文件没变，不需要重新计算归因）
```

> **新方案下没有 dirty_files**
>
> 新方案（Native Hooks）中，VS Code 传来的 JSON 不包含 `dirty_files`。
> Rust 直接从**磁盘**读取文件内容。
> 这是可靠的，因为 PostToolUse 触发时 VS Code 已经把修改写入磁盘了。

#### 补充：PreToolUse / PostToolUse 只传文件路径，git-ai 怎么拿到修改前后的内容？

这是源码里一个很容易误解的点：**VS Code hook 传给 git-ai 的主要是“这次该看哪些文件”，不是把 before / after 两份完整文本都直接塞给 git-ai。**

真正的 before / after 内容，是 Rust 自己在本地重建出来的。

```
收到文件路径（例如 src/main.rs）
  │
  ├─ 先取 after（当前版本）
  │   └─ read_current_file_content(file_path)
  │      • 优先读 dirty_files 缓存（旧方案 / 特殊场景）
  │      • 否则直接按文件路径读取当前工作区文件内容
  │
  └─ 再取 before（上一版本）
    ├─ 如果这个文件之前已经进过 working log
    │   └─ 用上一条 checkpoint 里记录的 blob_sha
    │      从 blobs/{sha256} 还原上一次文件内容
    │
    ├─ 如果之前没有 checkpoint
    │   └─ 从当前 HEAD commit 的 tree 里读取该文件旧内容
    │
    └─ 如果这个文件带有 INITIAL 初始归因
      └─ 再结合 INITIAL 里的 file_blobs / 快照内容
         修正“真正的起始版本”
```

所以这里并不是：

- PreToolUse 直接给了一份 before 文本
- PostToolUse 直接给了一份 after 文本

而是：

- Hook 只告诉 git-ai “该关注哪些文件”
- git-ai 自己按路径去本地文件系统、working log、HEAD、INITIAL 中取内容
- 最后再把这些内容交给 diff / attribution 流程处理

源码对应关系可以概括成这样：

- 当前版本（after）：`read_current_file_content()`
- 上一个 checkpoint 版本：`get_file_version(blob_sha)`
- Git 已提交基线：`get_previous_content_from_head()`
- 继承的初始快照：`initial_file_content_from()` / `stored_initial_file_content_from()`

这也是为什么新方案里即使 hook 只传文件路径，Rust 侧仍然能完成精确归因：因为真正的“内容源”在本地仓库和 working log 中，而不是完全依赖 VS Code 把整份文本传过来。

---

### 步骤三：对每个文件做字符级 diff 归因

这是整个系统最核心、最复杂的部分。函数 `make_entry_for_file()` 执行三步转换。

> **什么是"字符级"？**
>
> 普通的 git diff 告诉你"这一行被改了"。
> 但 Git AI 需要更精确：它要知道"这一行里哪些字符是 AI 改的，哪些是人类打的"。
> 所以它不是按"行"来追踪，而是按"字符"来追踪——这就是"字符级"。
> 比如一行有 80 个字符，前 20 个是人类打的（变量声明），后 60 个是 AI 补全的（函数体），
> 系统会分别标记 "字符0-20=人类" 和 "字符20-80=AI"。

#### 第一步：填充未归因的字符范围

函数 `attribute_unattributed_ranges()` 做的事：

```
输入：
  - 上一版本的文件内容
  - 上一版本已有的归因列表
  - 默认作者（固定为 "human"）
  - 时间戳（当前时间 - 1 毫秒，确保比即将到来的新归因旧）

处理过程：
  逐字符扫描文件内容
  对每个字符位置检查：是否被某个已有归因覆盖了？
  
  如果被覆盖 → 保留原有归因
  如果未被覆盖 → 记录这个"空白区间"
  
  扫描完成后，把所有空白区间标记为 "human"

输出：
  完整的归因列表（文件里的每一个字符都有归属）
```

**为什么要先做这一步？**

因为上一版本的归因列表可能不完整。比如：
- 文件是新加入追踪的，之前没有任何归因
- 上一版本只记录了被 AI 改过的部分，人类写的部分没有专门记录

这一步确保"没有证据是 AI 的 = 默认是人类"这个原则得到贯彻。

#### 第二步：用 diff 计算新归因

函数 `update_attributions_for_checkpoint()` 是最复杂的函数，分 5 个阶段：

```
阶段 1：计算 diff
──────────────
  使用 imara_diff 库（和 git diff 相同的 Myers 算法）
  
  什么是 Myers 算法？
  ──────────────────
  这是一种由 Eugene W. Myers 在 1986 年发明的算法
  专门用来找出两段文本之间的最小差异
  它会尽可能少地标记"删除"和"插入"，让 diff 结果最精简
  几乎所有 git 工具用的都是这个算法
  你不需要理解算法细节，只需要知道：
  "给它旧文件和新文件，它告诉你哪些部分变了"
  
  输入：旧内容、新内容
  输出：字节级的差异操作列表
  
  差异操作有三种：
  • Equal（相等）：这段文字没变
  • Delete（删除）：这段文字在旧版本里有，新版本里没了
  • Insert（插入）：这段文字在新版本里新增的
  
  举例：
  旧文件："ABCDEF"
  新文件："ABXYZF"
  diff 结果：
    Equal "AB"      ← A、B 没变
    Delete "CDE"    ← CDE 被删了
    Insert "XYZ"    ← XYZ 是新加的
    Equal "F"       ← F 没变

阶段 2：建立删除和插入目录
──────────────────────
  遍历差异操作列表
  记录每个删除段的字节位置和内容
  记录每个插入段的字节位置和内容
  
  结果类似：
  删除目录：[(旧文件第100-150字节, "这段被删了"), ...]
  插入目录：[(新文件第100-180字节, "这段是新加的"), ...]

阶段 3：检测"移动"操作
──────────────────
  ⭐ 这里 AI checkpoint 和 Human checkpoint 的处理方式不同！
  
  如果是 Human checkpoint：
    检查删除的文本和插入的文本是否高度相似
    如果相似 → 认为是"代码从一个位置移动到另一个位置"
    归因会跟着移动，保留原作者
    
    例如：你把一个函数从文件顶部移到底部
    系统会认出这是"移动"，而不是"删除旧函数 + 新增新函数"
    移动后的代码还是保留你的归因
  
  如果是 AI checkpoint：
    直接禁用移动检测！返回空的移动映射
    
    为什么？
    AI 经常做大段重构、格式化、代码重组
    虽然重写后的代码可能和原来很像
    但我们应该把重写区域算作 AI 的
    不应该因为"新代码和旧代码很像"就保留原作者
    
    这是一个有意识的产品设计选择：
    "AI 大段重写的代码，即使还像原来，也算 AI 的"

阶段 4：转换旧归因
────────────
  对每个旧的字符级归因：
  
  ├─ 如果它覆盖的文本被删除了：
  │   ├─ 有移动映射 → 归因跟着移动到新位置
  │   └─ 无移动映射 → 归因消失（文本不存在了，归因也没意义了）
  │
  ├─ 如果它覆盖的文本部分被修改了：
  │   → 根据 diff 调整字符位置（前面的删除/插入会造成偏移）
  │
  └─ 如果它覆盖的文本没变：
      → 保留，但可能需要调整字符位置（因为前面的其他改动导致偏移）
  
  对新插入的文本：
  检查新文件里哪些字符范围还没有被任何旧归因覆盖
  这些"无主"范围 → 标记为当前作者（current_author）
  
  current_author 在不同 checkpoint 下不同：
  • Human checkpoint → "human"
  • AiAgent checkpoint → agent_id 的短 hash（如 "copilot/abc123"）
  • AiTab checkpoint → "ai_tab"

阶段 5：合并相邻归因
────────────────
  相同作者的相邻字符范围 → 合并成一个
  例如：
    (0-50, human) + (50-100, human) → (0-100, human)
  
  减少存储量
```

**用一个具体例子说明：**

```
假设 main.rs 原来是这样（100 个字符）：
─────────────────────────
fn main() {
    println!("Hello, world!");
}
─────────────────────────
归因：字符 0-100 = "human", 时间戳 1000

Copilot 改成了这样（150 个字符）：
─────────────────────────
fn main() {
    let name = "Copilot";
    println!("Hello, {}!", name);
    println!("Welcome!");
}
─────────────────────────

diff 结果：
  字符 0-16: Equal    "fn main() {\n    "
  字符 16-45: Delete  "println!(\"Hello, world!\");"
  新增:       Insert  "let name = \"Copilot\";\n    println!(\"Hello, {}!\", name);\n    println!(\"Welcome!\");"

最终归因：
  字符 0-16:  "human", ts=1000            ← 未变部分保留原归因
  字符 16-120: "copilot/abc123", ts=2000   ← 新增部分标记为 AI
  字符 120-150: "human", ts=1000           ← "}\n" 未变，保留原归因（偏移后位置）
```

#### 第三步：字符级归因聚合成行级归因

这一步在下面专门讲解。

---

### 步骤四：字符级归因聚合成行级归因

函数 `attributions_to_line_attributions_for_checkpoint()` 把精确到字符的归因转成精确到行的归因。

```
第一遍：为每一行确定主导作者
──────────────────────────
  对文件的每一行（第1行、第2行、第3行……）：
  
  1. 找到这行的字符范围（比如第3行是字符 50-80）
  2. 找出所有与这个范围重叠的字符级归因
  3. 调用 find_dominant_author_for_line_candidates() 决定谁是主导作者

第二遍：合并连续的同作者行
──────────────────────
  例如：
    第1行: human
    第2行: human
    第3行: copilot/abc123
    第4行: copilot/abc123
    第5行: human
  
  合并为：
    第1-2行: human
    第3-4行: copilot/abc123
    第5行: human

第三遍：过滤——只保留有意义的归因
──────────────────────────────
  保留条件（满足其一即保留）：
  • author_id 不是 "human"（也就是是 AI 写的）
  • author_id 是 "human" 但 overrode 不为空（人类改写了 AI 代码）
  
  删除：
  • 纯 "human" 且没有 overrode 的行
  
  为什么删？
  ──────────
  Git AI 的目标不是记录"所有代码是谁的"
  而是记录"哪些代码是 AI 的，以及人类对 AI 代码做了什么修改"
  纯人类代码不需要特别记录——它是默认值
```

#### find_dominant_author_for_line_candidates() 的判定规则

这个函数决定"当多个作者的归因重叠在同一行时，谁是这行的主导作者"。

```
输入：
  - 这行的字符范围
  - 所有与这行重叠的归因列表
  
处理过程：

  1. 先过滤候选者
     ─────────────
     对每个重叠的归因：
     • 只看它在这行范围内的那部分字符
     • 检查这些字符是不是"非空白字符"
     
     保留规则（满足任一即保留）：
     • 有非空白字符（真正有内容的归因）
     • 这行本身是空行
     • 这是一个删除标记（start == end，表示有东西在这被删了）
     • 这是 AI checkpoint 中的 AI 归因（AI 的空白也算）
     
     为什么纯空白一般被丢弃？
     因为代码里的缩进空格、空行等是"格式"而非"内容"
     按空白来判断归因没有意义
  
  2. 选择主导作者——最新时间戳胜出
     ──────────────────────────────
     遍历所有保留的候选归因
     选时间戳（ts）最大的那个作者
     
     例如：
     • 归因A: author="human", ts=1000
     • 归因B: author="copilot/abc", ts=2000
     → 胜出者 = "copilot/abc"（因为 2000 > 1000）
     
     反过来：
     • 归因A: author="copilot/abc", ts=1000
     • 归因B: author="human", ts=2000
     → 胜出者 = "human"（因为 2000 > 1000）
  
  3. 计算 overrode 字段
     ──────────────────
     检查这行同时有没有 AI 归因和 human 归因
     
     场景1：AI 后于 human 编辑
       last_ai.ts > last_human.ts
       → overrode = None
       （AI 是最终版本，没有"覆盖"关系需要记录）
     
     场景2：human 后于 AI 编辑
       last_human.ts > last_ai.ts
       → overrode = Some(ai_author_id)
       （人类覆盖了 AI 的编辑，记录被覆盖的 AI 作者）
     
     场景3：只有 AI 或只有 human
       → overrode = None
     
     overrode 的意义：
     ──────────────
     它回答的是："这行最终算人类的，但它曾经是 AI 写的"
     这让系统可以统计"人类修改了多少 AI 代码"
     而不仅仅是"现在是谁的"

输出：
  (主导作者ID, overrode)
  例如：("human", Some("copilot/abc123"))
  含义："这行现在算人类的，但它覆盖了 copilot/abc123 的编辑"
```

**用时间线举例：**

```
时间轴         操作              这行的归因状态
────────────────────────────────────────────────
t=100    AI 生成了这行        author="copilot/abc", ts=100
t=200    人类修改了这行        author="human", ts=200
                              → 主导作者 = human（200>100）
                              → overrode = "copilot/abc"
                              
t=300    AI 又重写了这行       author="copilot/abc", ts=300
                              → 主导作者 = copilot/abc（300>200）
                              → overrode = None（AI 是最新的）
```

---

### 人类手工修改代码是怎么检测的

这部分很容易和 Copilot 的 Native Hooks 混在一起，但源码里的路径其实是分开的。

**结论先说：人类手工修改并不是靠 VS Code 的 PreToolUse / PostToolUse 实时监听来检测，而是主要靠 git 的 `pre-commit` 钩子，在提交前做一次 `Human checkpoint`。**

流程如下：

```
你手工改代码
    │
    ├─ 平时只是改了工作区文件
    │   不会像 Copilot 那样自动触发 VS Code Native Hooks
    │
    └─ 当你执行 git commit
        │
        ├─ git 触发 pre-commit hook
        │
        ├─ git-ai 进入 pre_commit::pre_commit()
        │
        ├─ 调用 checkpoint::run(..., CheckpointKind::Human, ...)
        │
        └─ 对已暂存 / 将提交的文件做一次“人类归因更新”
           比较：
           • 当前文件内容
           • 上一次 checkpoint / HEAD / INITIAL 还原出来的旧内容
```

这一步的目标不是“记录所有人类代码”，而是更具体地回答两个问题：

1. 哪些原本是 AI 的代码，被人类后来改掉了？
2. 哪些带有历史 AI 归因的文件，在本次 commit 前又被人类改写了？

所以源码里还有一个非常重要的快速跳过条件：

```
如果同时满足：
  • 这是 Human checkpoint
  • 该文件历史上从未被 AI 触碰过
  • 也没有 INITIAL 继承归因

→ 直接跳过，不记录
```

为什么要这样做？

- 纯人类文件本来就是默认归因，不需要反复写“这还是人类写的”
- Git AI 关注的是“AI 写了什么，以及人类后来如何覆盖这些 AI 代码”
- 这样能把开销集中在真正需要追踪的文件上

换句话说，**人类编辑并不是完全不检测，而是“在 commit 前集中检测，并且只对和 AI 归因有关的文件认真计算”。**

这也解释了为什么你在平时手工敲代码时，不会看到像 Copilot 那样一前一后两次 checkpoint：

- Copilot 编辑：VS Code 在工具执行前后各触发一次 hook
- 人类编辑：通常等到 `git commit` 前，由 `pre-commit hook` 统一补一轮 `Human checkpoint`

---

## 提交阶段：归因数据怎么持久化

> **什么是"持久化"？**
>
> 程序在运行时产生的数据默认只存在内存（RAM）里，程序关闭就没了。
> "持久化"就是把数据写到磁盘上（硬盘/SSD），这样即使关机重启数据也还在。
> 这里"持久化"就是把归因结果保存到 git 仓库里，永久保留。
>
> **什么是 git commit（提交）？**
>
> git 是程序员用来管理代码版本的工具。每次你觉得改得差不多了，
> 就执行 `git commit` 命令，把当前改动保存成一个"版本"。
> 每个版本有一个唯一的 ID（叫 SHA，是一串 40 位的十六进制字符，比如 `a1b2c3d4...`）。
>
> **什么是 git hook（git 钩子）？**
>
> 和前面讲的 VS Code 钩子是同一个概念，只是挂在 git 上。
> "post-commit hook"就是"每次 git commit 完成后，自动执行的一段命令"。
> Git AI 在安装时会设置这个钩子，所以每次你提交代码，它都会自动处理归因数据。

### 编辑到提交之间：Working Log

每次 checkpoint 产生的归因数据不是直接写到 git 里，而是暂存在本地的 Working Log 中。

> **为什么不直接写进 git？**
>
> 因为你在两次 commit 之间可能编辑很多次——
> Copilot 改了一下，你又改了一下，Copilot 又改了一下……
> 每次 checkpoint 都会更新归因。如果每次都往 git 里写，太频繁了。
> 所以先暂存在本地的 Working Log 里，等你 commit 时一次性汇总写入。

这意味着：**Working Log 只属于当前机器、当前本地工作状态。**
它不是跨机器同步的数据。A 机器上 `.git/ai/working_logs/` 里的未提交归因，不会随着普通 `git push` 自动变成 B 机器上的 working log。

```
存储位置：.git/ai/working_logs/{base_commit_sha}/
    │
    ├─ checkpoints.jsonl    ← 每行一个 JSON，记录每次 checkpoint 的完整信息
    │   内容包括：
    │   • checkpoint_kind (Human/AiAgent/AiTab)
    │   • 每个文件的字符级归因列表
    │   • 每个文件的行级归因列表
    │   • agent_id（工具、会话ID、模型）
    │   • 对话记录
    │   • 时间戳
    │
    ├─ blobs/{sha256}       ← 文件内容快照（用 SHA256 去重）
    │   让系统能回溯"上一个 checkpoint 时文件长什么样"
    │
    └─ INITIAL              ← 初始归因数据（从上一次 commit 继承来的）
       如果文件在上一次 commit 时就有 AI 归因
       这些信息会在新的 working log 里通过 INITIAL 文件保留
```

  这里要特别注意：上面这个路径是**普通仓库的简化写法**，linked worktree 的真实路径会落到 `git common dir` 下；而且 `working_logs` 顶层目录在实际使用中**完全可能是空的**，这通常不代表异常，只说明当前没有可保留的 working log。最常见的原因是：

  - 这次还没有触发任何 AI checkpoint
  - 你已经完成了 commit，旧的 `{base_commit_sha}` 目录已被清理
  - 当前没有需要继承到下一轮的旧 AI 归因，所以不会留下 `INITIAL`

> **什么是 `base_commit_sha`？**
>
> 就是当前这批编辑的"起点" commit 的 ID。
> 例如你在 commit `a1b2c3` 之后开始改代码，那 `base_commit_sha` 就是 `a1b2c3`。
> Working Log 文件夹的名字就是这个 ID，记录的是从 `a1b2c3` 到下次 commit 之间的所有 checkpoint。

> **什么是 `.jsonl` 文件？**
>
> `.jsonl`（JSON Lines）就是一个文本文件，每一行是一个独立的 JSON 对象。
> 比如第一行记录了 t=100 时的 checkpoint，第二行记录了 t=200 时的 checkpoint，依此类推。
> 好处是可以一行一行追加，不需要像普通 JSON 那样把整个文件重新写一遍。

> **什么是 SHA256？**
>
> SHA256 是一种"哈希算法"。给它任何内容，它会算出一个固定长度的"指纹"（64 个十六进制字符）。
> 相同的内容永远得到相同的指纹，不同的内容（几乎）不可能得到相同指纹。
> Git AI 用它来给文件内容编号：把文件内容丢进 SHA256 → 得到一个指纹 → 用这个指纹做文件名。
> 这样相同内容的文件只保存一份（去重），节省磁盘空间。

### git commit 时：Post Commit

当你执行 `git commit` 时，Git AI 的 post-commit hook 会自动触发 `post_commit_with_final_state()` 函数：

```
post_commit 流程
    │
    ├─ 第1步：读取 Working Log
    │   从 .git/ai/working_logs/{parent_sha}/ 读取所有 checkpoint
    │
    ├─ 第2步：构建虚拟归因
    │   VirtualAttributions 把所有 checkpoint 的归因叠加
    │   按时间戳顺序处理，后来的覆盖先前的
    │
    ├─ 第3步：生成最终的 AuthorshipLog
    │   包含：
    │   • 每个文件的行级归因（哪些行是 AI 的，哪些是人类覆盖了 AI）
    │   • AI 会话元数据（agent_id、模型名称）
    │   • 对话记录（如果配置了保留）
    │   • 统计数据（各类型的增减行数）
    │
    ├─ 第4步：序列化并写入 git notes
    │   authorship_json = 把 AuthorshipLog 序列化成 JSON 字符串
    │   notes_add(repo, commit_sha, authorship_json)
    │   │
    │   git notes 是什么？
    │   ──────────────────
    │   git 内置的机制，可以给任何 commit 附加额外数据
    │   存储在 refs/notes/ai 这个特殊引用下
    │   不会改变 commit 本身的 hash
    │   可以用 git notes show <commit> 查看
    │   在磁盘上它通常对应真实 Git 目录里的 `.git/refs/notes/ai`
    │   但它也可能被 Git 打包进 `.git/packed-refs`，所以未必会看到一个单独的 ai 文件
    │   可以推送到远程仓库（git push origin refs/notes/ai）
    │   如果启用了 git-ai 管理的 push hooks，普通 git push 时也会尝试自动同步这份 refs/notes/ai
    │   但如果只是裸 git push、没有启用这些 hooks，Git 默认并不会保证把 notes 一起推上去
    │   只有这份 notes 也被同步到远程、并在另一台机器 fetch 下来后
    │   另一台机器才看得到这些已提交归因
    │
    ├─ 第5步：继承归因到下一个 Working Log
    │   如果某些文件有 AI 归因需要跨 commit 保留
    │   把它们写入新的 .git/ai/working_logs/{new_commit_sha}/INITIAL
    │
    └─ 第6步：清理旧的 Working Log
       删除 .git/ai/working_logs/{parent_sha}/ 目录
```

    所以如果按你的 A / B 场景来理解，正确说法是：

    - **未 commit 的归因**：在 A 机器本地的 `working_logs` 里，只对 A 自己可见，不会直接传到 B
    - **已 commit 的归因**：写进 `refs/notes/ai` 后，可以被同步到远程，再被 B fetch 下来
      这里的"可以被同步"，在默认使用 git-ai hooks 的场景下通常会随 push / pull 自动处理；
      但如果没有启用这些 hooks，就需要显式同步 notes，而不能只假设代码 push 了 notes 也一定跟着走
    - **B 能否在 A 的基础上继续归因**：可以，但前提是 B 不只是拉到了代码 commit，还拿到了对应的 `refs/notes/ai`

    如果 B 只拉到了代码、没拉到 notes，那么 B 看到的是代码内容，但看不到 A 已提交的归因历史；这样后续归因就无法完整继承 A 的结果。

---

## 完整流程图

新方案（Native Hooks，VS Code ≥ 1.109.3）从用户编辑到最终归因落盘的完整流程：

```
                       用户在 VS Code 里用 Copilot Chat 改代码
                                       │
                                       ▼
                ┌──────────────────────────────────────────┐
                │  AI 大模型返回工具调用指令                  │
                │  例如：editFile { file_path: "main.rs" }  │
                └──────────────────────┬───────────────────┘
                                       │
              VS Code 准备执行工具，先触发 PreToolUse 钩子
                                       │
                                       ▼
         ┌─────────────────────────────────────────────────────┐
         │  VS Code 读取 ~/.copilot/hooks/git-ai.json           │
         │  spawn: git-ai checkpoint github-copilot             │
         │         --hook-input stdin                           │
         │  stdin = {                                           │
         │    hookEventName: "PreToolUse",                      │
         │    toolName: "editFile",                             │
         │    toolInput: { file_path: "main.rs" },              │
         │    sessionId: "xxx-xxx",                             │
         │    transcript_path: "/path/to/session.jsonl",        │
         │    cwd: "/your/project"                              │
         │  }                                                   │
         └──────────────────────┬──────────────────────────────┘
                                │
                                ▼
         ┌─────────────────────────────────────────────────────┐
         │  Rust: GithubCopilotPreset 解析 PreToolUse           │
         │  • 过滤非编辑工具（search/read 等直接退出）            │
         │  • 提取 will_edit_filepaths                          │
         │  • checkpoint_kind = Human                           │
         │  → AgentRunResult { kind=Human, files, ... }         │
         └──────────────────────┬──────────────────────────────┘
                                │
                                ▼
         ┌─────────────────────────────────────────────────────┐
         │  Rust: checkpoint.rs 拍"编辑前"基线快照              │
         │  对 will_edit_filepaths 里每个文件：                  │
         │    读当前磁盘内容 → 存入 working log 作为基线          │
         └──────────────────────┬──────────────────────────────┘
                                │
              VS Code 真正执行工具（Copilot 编辑文件）
                                │
                                ▼
         ┌─────────────────────────────────────────────────────┐
         │  编辑完成，VS Code 触发 PostToolUse 钩子              │
         │  stdin = {                                           │
         │    hookEventName: "PostToolUse",                     │
         │    toolName: "editFile",                             │
         │    toolInput: { file_path: "main.rs" },              │
         │    sessionId: "xxx-xxx",                             │
         │    transcript_path: "/path/to/session.jsonl",        │
         │    cwd: "/your/project"                              │
         │  }                                                   │
         └──────────────────────┬──────────────────────────────┘
                                │
                                ▼
         ┌─────────────────────────────────────────────────────┐
         │  Rust: GithubCopilotPreset 解析 PostToolUse          │
         │  • 提取 edited_filepaths                             │
         │  • 验证 transcript_path 是 Copilot 的（排除 Claude） │
         │  • 读会话文件，解析模型名（如 copilot/claude-sonnet-4）│
         │  • checkpoint_kind = AiAgent                         │
         │  → AgentRunResult { kind=AiAgent, agent_id, files }  │
         └──────────────────────┬──────────────────────────────┘
                                │
                                ▼
         ┌─────────────────────────────────────────────────────┐
         │  Rust: checkpoint.rs 处理每个文件                     │
         │                                                      │
         │  步骤 1: attribute_unattributed_ranges()              │
         │    → 未归因字符默认标记为 "human"                      │
         │                                                      │
         │  步骤 2: update_attributions_for_checkpoint()         │
         │    → imara_diff 对比基线内容 vs 磁盘当前内容            │
         │    → AI checkpoint 禁用移动检测                       │
         │    → 新增字符标记为当前 AI 作者（copilot/xxx）          │
         │    → 未变字符保留原归因                                │
         │                                                      │
         │  步骤 3: attributions_to_line_attributions()          │
         │    → 字符级 → 行级（时间戳最新的作者胜出）              │
         │    → 过滤掉纯人类行，只保留 AI 行和人类覆盖 AI 行       │
         └──────────────────────┬──────────────────────────────┘
                                │
                                ▼
         ┌─────────────────────────────────────────────────────┐
         │  写入 Working Log                                     │
         │  .git/ai/working_logs/{base_commit}/                 │
         │    checkpoints.jsonl ← 追加一行                       │
         │    blobs/{sha256}    ← 保存文件快照                   │
         └──────────────────────┬──────────────────────────────┘
                                │
           （可能还有更多 Copilot 工具调用，每次都重复以上循环）
                                │
                                │  ← 人类手动修改代码（不触发以上流程）
                                │
                          用户执行 git commit
                                │
                    ┌───────────┴───────────┐
                    ▼                       ▼
         ┌────────────────┐     ┌───────────────────────────────┐
         │ pre-commit hook│     │  post-commit hook 触发         │
         │ 由 git 触发    │     │                               │
         │                │     │  汇总所有 checkpoint           │
         │ checkpoint.rs  │     │  → AuthorshipLog              │
         │ kind=Human     │     │  序列化 JSON                   │
         │ 对已暂存文件    │     │  → notes_add()                │
         │ 做一次人类归因  │     │  写入 refs/notes/ai            │
         │ 记录人类改写了  │     │  清理旧 Working Log            │
         │ 哪些 AI 代码   │     │                               │
         └───────┬────────┘     │  最终：commit 的 git note 里   │
                 │              │  记录了每行是谁写的              │
                 └──────────────┤  以及人类覆盖 AI 的痕迹         │
                                └───────────────────────────────┘
```

---

## 附：关键数据结构一览

### Attribution（字符级归因）

```
字段          类型       含义
───────────────────────────────────────────────────
start         数字       起始字符位置（从0开始，包含）
end           数字       结束字符位置（不包含）
author_id     字符串     作者标识
                         • "human" = 人类
                         • "copilot/abc123" = 某次 Copilot 会话
                         • "ai_tab" = Tab 补全
ts            数字       时间戳（毫秒，从 epoch 开始）
                         决定"谁后改的"

特殊情况：
• start == end 时是"删除标记"，表示这个位置有文本被删除过
• ts 值 42 是保留值，代表从 git restore 恢复的文件
```

### LineAttribution（行级归因）

```
字段          类型       含义
───────────────────────────────────────────────────
start_line    数字       起始行号（从1开始，包含）
end_line      数字       结束行号（包含）
author_id     字符串     主导作者（时间戳最新的获胜）
overrode      字符串?    被覆盖的作者（可选）
                         如果不为空，说明人类改写了 AI 代码
                         值是被覆盖的那个 AI 作者的 ID
```

### CheckpointKind（检查点类型）

```
值            内部字符串     含义
───────────────────────────────────────────────────
Human         "human"       人类编辑（或 AI 编辑前的人工基线）
AiAgent       "ai_agent"    AI 聊天编辑（Copilot Chat 等）
AiTab         "ai_tab"      AI Tab 补全（按 Tab 接受的建议）
KnownHuman    "known_human" VS Code 扩展在用户保存时主动声明的人类编辑
                            （is_ai() 仍返回 false，但带 KnownHumanMetadata）
```

### KnownHumanMetadata（人类主动保存时的元数据）

```
字段                含义
───────────────────────────────────────────────────
editor              触发的编辑器名称（如 "vscode"）
editor_version      编辑器版本
extension_version   git-ai 扩展的版本
```

### AgentId（AI 代理标识）

```
字段          含义
───────────────────────────────────────────────────
tool          AI 工具名，如 "github-copilot"、"claude"
id            会话 UUID，如 "01f62e6b-9812-4964-b9a6-c4fd0ce15fa2"
model         使用的模型，如 "copilot/claude-sonnet-4"、"copilot/gpt-4o"
```

### WorkingLogEntry（工作日志条目——每个文件一条）

```
字段               含义
───────────────────────────────────────────────────
file              文件路径（相对于仓库根目录）
blob_sha          文件内容的 SHA256 哈希（用于去重和回溯）
attributions      字符级归因列表（Attribution[]）
line_attributions 行级归因列表（LineAttribution[]）
```

### 存储路径总结

```
普通仓库：
  .git/ai/
   └─ working_logs/
      └─ {base_commit_sha}/
        ├─ checkpoints.jsonl    ← 所有 checkpoint 记录
        ├─ blobs/{sha256}       ← 文件内容快照
        └─ INITIAL              ← 仅在需要继承旧 AI 归因时才会出现

linked worktree：
  {git_common_dir}/ai/worktrees/{worktree_name}/working_logs/{base_commit_sha}/
        ├─ checkpoints.jsonl
        ├─ blobs/{sha256}
        └─ INITIAL

refs/notes/ai                     ← 最终归因数据（JSON，附在 commit 上）
```

这里最容易误解的有三点：

1. 这个路径是 **Git 实际目录** 下的内部存储路径，不一定就是你工作区根目录里肉眼可见的 `.git/` 文件夹。
  - 普通仓库里，通常就是工作区下的 `.git/ai/...`
  - 但如果你用的是 `git worktree`，工作区根目录里的 `.git` 往往只是一个文本文件，里面写着 `gitdir: ...`
  - 这种情况下，Git AI 不把数据存到工作区那个 `.git` 文件旁边，而是存到 **common git dir** 下的 `ai/worktrees/{worktree_name}/...`

2. `INITIAL` **不是每次都会有**。
  它只在"上一个 commit 已经存在 AI 归因，且这些归因需要延续到新的 working log 周期"时才会写出。
  如果当前仓库还没有可继承的 AI 归因，或者这批改动里没有需要沿用的旧 AI 记录，就不会生成 `INITIAL`。

3. `{base_commit_sha}` 这一层目录是 **临时工作目录**，不是长期归档目录。
  checkpoint 发生时会写到这里；但 commit 完成后，旧的 `working_logs/{parent_sha}` 会被删除。
  只有在 commit 后仍有未提交、且需要继承的 AI 归因时，才会在新的 `working_logs/{new_commit_sha}/INITIAL` 里保留一份起始状态。

所以你在实际使用时没看到这个路径，常见原因通常只有这几种：

- 你看的不是 Git 实际目录，而是 worktree 根目录里的 `.git` 指针文件
- 你查看时已经 commit 结束，旧的 `working_logs/{base_commit}` 已经被清掉了
- 当前没有可继承的旧 AI 归因，所以根本不会生成 `INITIAL`
- 还没有产生任何 checkpoint，`working_logs/{base_commit}` 目录尚未被实际用到

如果你想在本机直接定位真实目录，最稳妥的办法是先看这两个命令：

```bash
git rev-parse --git-dir
git rev-parse --git-common-dir
```

- 普通仓库：通常看 `$(git rev-parse --git-dir)/ai/working_logs`
- linked worktree：通常看 `$(git rev-parse --git-common-dir)/ai/worktrees/{当前 worktree 名}/working_logs`
