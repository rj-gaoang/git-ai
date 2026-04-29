# git-ai 安装后自动上传链路的数据库只读验证手册

> 状态：可执行验证文档
> 日期：2026-04-26
> 适用范围：已经安装 `git-ai`，需要验证“用户在本地 commit 后，归因数据会自动上报到远端服务并最终落库”
> 验证边界：验证过程中只允许对数据库执行查询操作，不允许任何写操作

---

## 1. 这份方案解决什么问题

当前最核心的验证场景，不是源码能不能编译，而是下面这条真实用户路径是否成立：

1. 用户本机已经安装 `git-ai`。
2. 仓库已经执行过 `git-ai install`，commit 时会生成 note 和 stats。
3. 当前版本默认就会自动上传；如果现场要显式覆盖，才设置 `GIT_AI_AUTO_UPLOAD_AI_STATS=true/false`。
4. 远端服务成功接收，并把这次 commit 的归因数据落到数据库。
5. 验证人员只通过数据库查询，就能证明这条链路已经打通。

这意味着：

1. 本文把“数据库只读核验”作为主线。
2. 源码级 build/test 仍然有价值，但不再是主验证路径。
3. 只要能用 `commitSha` 在数据库里串起 summary、commit、file、tool 四层数据，就可以判定远端链路已成功落库。

---

## 2. 已确认的数据库对象

本次已实际连通数据库并确认以下事实：

1. 数据库类型是 MySQL 8.0。
2. 目标 schema 是 `cr`。
3. 相关落库表如下：

```text
cr.git_ai_summary_stats
cr.git_ai_commit_stats
cr.git_ai_file_stats
cr.git_ai_tool_stats
cr.git_ai_prompt_stats
```

4. 关键关联关系如下：

```text
git_ai_commit_stats.summary_id = git_ai_summary_stats.id
git_ai_file_stats.commit_id = git_ai_commit_stats.id

git_ai_tool_stats.source_type = 0 时，source_id = git_ai_commit_stats.id
git_ai_tool_stats.source_type = 1 时，source_id = git_ai_file_stats.id
git_ai_prompt_stats.commit_id = git_ai_commit_stats.id
```

6. `git_ai_tool_stats` 中的 `tool` 和 `model` 本来就是分列保存；如果某次验证里查不到提交级工具行，优先怀疑的是上传 payload 没带出 commit 级工具信息，而不是表结构缺列。

5. 当前自动上传链路的 `source` 约定是：

```text
auto
```

因此，验证自动上传时不要拿 `manual` 或 `codeReview` 的历史数据当成功样本。

---

## 3. 验证原则

### 3.1 只做数据库查询

允许的数据库操作只有：

1. `SELECT`
2. `SHOW`
3. `DESC` / `DESCRIBE`

不允许做的事情：

1. `INSERT`
2. `UPDATE`
3. `DELETE`
4. `REPLACE`
5. `TRUNCATE`
6. 任何建表、改表、删表操作

### 3.2 不把数据库密码写进仓库文档

数据库连接信息可以在现场使用，但密码不要直接写入 Markdown 文件，也不要落到脚本仓库里。推荐在终端交互输入密码：

```powershell
mysql -h k8s-bj-pro-nodeports.ruijie.com.cn -P 31989 -u system -p
```

进入客户端后再执行：

```sql
USE cr;
```

### 3.3 先看 commitSha，再看数据库

数据库验证必须围绕“刚刚产生的那次 commit”来做，而不是只看最近几条记录。否则很容易把旧数据误判成这次成功。

---

## 4. 最短验证路径

如果你只关心“安装后的 git-ai 是否真的能把 commit 归因数据自动落库”，建议只走下面 6 步。

### 4.1 确认当前 shell 调到的是目标 git-ai

```powershell
git-ai --version
Get-Command git-ai | Format-List Source
```

通过标准：

1. `git-ai --version` 有输出。
2. `Get-Command git-ai` 指向你预期的安装路径。

### 4.2 在目标仓库安装 hook

```powershell
cd <你的目标仓库>
git-ai install
```

如果你不想一上来就在真实业务仓库里验证，建议先新建一个最小仓库：

```powershell
mkdir D:\tmp\git-ai-auto-upload-e2e -Force
cd D:\tmp\git-ai-auto-upload-e2e

git init
git branch -M main
git remote add origin https://github.com/example/git-ai-auto-upload-e2e.git
git-ai install
```

### 4.3 准备自动上传环境变量

```powershell
$env:GIT_AI_REPORT_REMOTE_API_KEY = "<现场填写>"
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
$env:GIT_AI_REPORT_REMOTE_USER_ID = "<现场填写>"
$env:GIT_AI_DEBUG = "1"
```

说明：

1. 当前版本默认已开启自动上传，所以上面不再把 `GIT_AI_AUTO_UPLOAD_AI_STATS` 作为必需项。
2. 如果现场要强制打开或关闭，请显式设置 `GIT_AI_AUTO_UPLOAD_AI_STATS=true` 或 `false`。
3. 不要使用 `GIT_AI_AUTO_UPLOAD_AI_STATS=1`，运行时不会把它识别成开启。
4. 首次联调建议显式设置 `GIT_AI_REPORT_REMOTE_USER_ID`，不要依赖 MCP 兜底。
5. `GIT_AI_DEBUG=1` 能让你在本地看到上传成功或失败日志。

### 4.4 先做一次本地前置校验

先做一个很小的 commit，只确认 note 和 stats 正常：

```powershell
@'
fn main() {
    println!("hello");
}
'@ | Set-Content .\main.rs

git add .
git commit -m "test: note and stats"

git notes --ref=ai list HEAD
git-ai stats HEAD --json
```

通过标准：

1. `git notes --ref=ai list HEAD` 有输出。
2. `git-ai stats HEAD --json` 返回合法 JSON。

如果这一步没过，不要继续验证上传，因为远端上传依赖这里已经算出来的统计结果。

### 4.5 再做一次用于自动上传的验证 commit

```powershell
Add-Content .\main.rs "`nfn added_for_upload_test() {}"

git add .
git commit -m "test: native auto upload"
```

记录这次 commit 的完整 SHA：

```powershell
$CommitSha = (git rev-parse HEAD).Trim()
$CommitSha
```

本地期望日志：

```text
[git-ai] upload-ai-stats: uploaded stats for <short_sha>
```

注意：commit 成功本身不等于上传成功，最终要以数据库查询结果为准。

### 4.6 用数据库只读查询确认落库

连接数据库：

```powershell
mysql -h k8s-bj-pro-nodeports.ruijie.com.cn -P 31989 -u system -p
```

进入库：

```sql
USE cr;
```

然后把下面的 `<完整commit sha>` 替换成刚才输出的 SHA。

---

## 5. 数据库只读核验 SQL

### 5.1 先做一个最快速的存在性检查

```sql
SELECT COUNT(*) AS commit_row_count
FROM cr.git_ai_commit_stats
WHERE commit_code = '<完整commit sha>';
```

通过标准：

1. `commit_row_count >= 1`

如果这里就是 0，说明远端没有落到 `git_ai_commit_stats`，后面的明细查询就没有必要继续查了。

### 5.2 核对 summary + commit 主记录

```sql
SELECT
    s.id AS summary_id,
    s.serial_number,
    s.project_name,
    s.repo_url,
    s.branch,
    s.source,
    s.authorship_schema_version,
    s.review_document_id,
    s.create_time AS summary_create_time,
    c.id AS commit_id,
    c.commit_code,
    c.commit_message,
    c.commit_time,
    c.author,
    c.total_add_line,
    c.ai_add_line,
    c.manual_add_line,
    c.toal_del_line,
    c.ai_del_line,
    c.manual_del_line,
    c.wait_time,
    c.create_time AS commit_create_time
FROM cr.git_ai_summary_stats s
JOIN cr.git_ai_commit_stats c ON c.summary_id = s.id
WHERE c.commit_code = '<完整commit sha>'
ORDER BY s.create_time DESC, c.create_time DESC;
```

这条 SQL 用来回答 6 个问题：

1. 这次 commit 是否真的落库。
2. `repo_url` 是否是当前仓库。
3. `branch` 是否是当前分支。
4. `source` 是否等于 `auto`。
5. `author`、`commit_time` 是否与本地 commit 对得上。
6. `total_add_line`、`ai_add_line`、`manual_add_line` 是否与本地 `git-ai stats HEAD --json` 大致一致。

通过标准：

1. 至少返回 1 行。
2. `commit_code` 与本地 SHA 完全一致。
3. `source = 'auto'`。
4. `repo_url`、`branch`、`author` 与本地环境一致。

### 5.3 核对文件级统计

```sql
SELECT
    f.id AS file_id,
    f.class_name,
    f.total_add_line,
    f.ai_add_line,
    f.manual_add_line,
    f.total_del_line,
    f.ai_del_line,
    f.manual_del_line,
    f.create_time
FROM cr.git_ai_file_stats f
JOIN cr.git_ai_commit_stats c ON c.id = f.commit_id
WHERE c.commit_code = '<完整commit sha>'
ORDER BY f.id ASC;
```

通过标准：

1. 如果这次 commit 只改了 1 个文件，通常应只返回 1 行。
2. `class_name` 要能对应到你本地实际修改的文件。
3. `total_add_line`、`ai_add_line`、`manual_add_line` 应与本地 `stats.files[]` 基本一致。

### 5.4 核对提交级工具统计

```sql
SELECT
    c.commit_code,
    t.tool,
    t.model,
    t.add_line,
    t.del_line,
    t.create_time
FROM cr.git_ai_tool_stats t
JOIN cr.git_ai_commit_stats c ON c.id = t.source_id
WHERE t.source_type = 0
  AND c.commit_code = '<完整commit sha>'
ORDER BY t.add_line DESC, t.id ASC;
```

说明：

1. `source_type = 0` 代表提交级工具统计。
2. `tool` 和 `model` 会分列保存，比如 `tool='github copilot'`、`model='gpt 5.4'`。
3. 当前服务端除了兼容 `stats.toolModelBreakdown`，还会根据 commit 级 `prompts[]` 回填提交级工具统计，因此新验证数据应优先查这里。
4. 如果你查的是历史数据，或者远端服务还没升级到最新兼容逻辑，这里可能为空。

验证当前修复是否生效时，这条 SQL 很关键。

### 5.4.1 核对 prompt 明细

```sql
SELECT
    c.commit_code,
    p.prompt_hash,
    p.tool,
    p.model,
    p.human_author,
    p.prompt_text,
    p.messages_url,
    p.accepted_lines,
    p.overriden_lines,
    p.create_time
FROM cr.git_ai_prompt_stats p
JOIN cr.git_ai_commit_stats c ON c.id = p.commit_id
WHERE c.commit_code = '<完整commit sha>'
ORDER BY p.id ASC;
```

通过标准：

1. 如果 authorship note 里保存了 prompt 元数据，这里应至少返回 `prompt_hash/tool/model`。
2. 如果客户端同时保存了提示词摘要、完整消息或 `messagesUrl`，这里应能看到对应字段。
3. 这张表为空而 `git_ai_tool_stats` 也为空时，优先检查上传端是否真的带出了 commit 级 `prompts[]`。

### 5.5 核对文件级工具统计

```sql
SELECT
    c.commit_code,
    f.class_name,
    t.tool,
    t.model,
    t.add_line,
    t.del_line,
    t.create_time
FROM cr.git_ai_tool_stats t
JOIN cr.git_ai_file_stats f ON f.id = t.source_id
JOIN cr.git_ai_commit_stats c ON c.id = f.commit_id
WHERE t.source_type = 1
  AND c.commit_code = '<完整commit sha>'
ORDER BY f.class_name, t.add_line DESC, t.id ASC;
```

说明：

1. `source_type = 1` 代表文件级工具统计。
2. 这条查询已经在真实数据库样本上验证过，可直接使用。

通过标准：

1. 对于有 AI 改动的文件，应能查到对应 `tool/model/add_line`。
2. `tool`、`model` 应与本地生成该改动时实际使用的工具大体一致。

---

## 6. 一次性验收标准

只要下面 8 条全部成立，就可以认为“安装后的 git-ai 自动上传链路已经验证通过”：

1. `git-ai --version` 正常，且当前 shell 指向预期安装路径。
2. 仓库已执行 `git-ai install`。
3. 本地前置 commit 能生成 note，`git-ai stats HEAD --json` 正常。
4. 开启 `GIT_AI_AUTO_UPLOAD_AI_STATS=true` 后，再做一个小 commit。
5. 本地能拿到这次 commit 的完整 `commitSha`。
6. `git_ai_commit_stats` 能按 `commit_code = <commitSha>` 查到记录。
7. `git_ai_summary_stats` 联表后能看到 `source = 'auto'`，且 `repo_url`、`branch`、`author` 正确。
8. `git_ai_file_stats` 和 `git_ai_tool_stats` 能查到与本地改动相符的明细数据。

---

## 7. 常见失败与解释

### 7.1 本地 commit 成功，但数据库完全查不到 `commit_code`

最常见原因：

1. 没开 `GIT_AI_AUTO_UPLOAD_AI_STATS=true`，或者错误地把它设成了 `"1"`。
2. `API Key`、`URL`、`USER_ID` 配置不对。
3. 当前 shell 实际调用到的不是你期望的 `git-ai`。
4. 这次 commit 被判定为 merge commit 或 expensive skip。

### 7.2 能查到 `git_ai_commit_stats`，但 `source` 不是 `auto`

这通常说明你查到的是手动上传或 Code Review 上传的数据，不是这次自动上传的数据。优先按完整 `commitSha` 精确过滤，而不是只按时间范围看最近记录。

### 7.3 能查到 summary/commit，但没有 file 明细

这说明主记录已经落库，但文件级明细没有写入。优先回查本地 `git-ai stats HEAD --json` 是否本身就没有 `files[]`，其次再看服务端是否处理了 `stats.files[]`。

### 7.4 文件级工具统计有数据，但提交级工具统计为空

分两种情况：

1. 如果你查的是历史数据，这可能是老版本服务端尚未兼容 `stats.toolModelBreakdown` 导致的正常现象。
2. 如果你查的是当前最新验证 commit，这通常说明客户端没有带出提交级 `toolModelBreakdown`，也没有带出 commit 级 `prompts[]`，或者远端服务还没升级到最新兼容版本。

### 7.5 `repo_url`、`branch`、`author` 对不上

这通常不是数据库问题，而是你在错误仓库、错误分支、错误 shell 里做了验证 commit，或者本地 git 身份配置不符合预期。

---

## 8. 推荐的最小查询顺序

如果你只想最快判断成败，不需要一次把所有明细都查完，建议按下面顺序：

1. 先查 `git_ai_commit_stats` 是否存在这条 `commitSha`。
2. 再查 summary + commit 联表，确认 `source = 'auto'`。
3. 再查 `git_ai_file_stats`，确认文件级统计已落库。
4. 再查 `git_ai_tool_stats`，看提交级和文件级工具统计是否完整。
5. 最后查 `git_ai_prompt_stats`，确认 prompt 明细有没有随同上传。

这个顺序的好处是：每一步都能明显缩小故障范围，不会把“完全没上传”和“部分字段没落库”混成同一个问题。

---

## 9. 附录：如果你验证的是源码改动，而不是安装态用户路径

如果你的目标不是“验证安装后的用户体验”，而是“验证本地改过的 `git-ai` 源码本身”，再额外补做下面这组检查：

```powershell
cd D:\git-ai-main\git-ai

cargo build --features test-support
cargo test --features test-support --lib integration::upload_stats -- --test-threads=1
cargo test --features test-support --lib feature_flags -- --test-threads=1
cargo clippy --features test-support --lib -- -D warnings
cargo fmt -- --check
```

但要注意：这组检查只能证明源码质量，不能替代本文的数据库只读落库验证。

---

## 10. 2026-04-26 实测补充

下面这部分不是“建议步骤”，而是本次已经实际执行过的验证记录。无论后续问题是否修复，这些记录都应该保留，避免重复验证同一个假设。

### 10.1 本次实测环境

1. 当前安装的 `git-ai` 版本是 `1.3.4`。
2. 可执行文件路径是 `C:\Users\15126\.git-ai\bin\git-ai.exe`。
3. `git-ai whoami` 状态是 `logged out`。
4. 数据库验证仍然严格限制为只读 SQL。

### 10.2 第一次试跑：工作区内嵌套仓库

首次为了快速打通链路，在工作区目录下创建了测试仓库：

```text
C:\Users\15126\Desktop\ruijie\git-ai-view\git-ai-auto-upload-e2e
```

这轮测试里，本地 note 和 stats 最终都能生成，但后台 daemon 日志出现了明确异常：

```text
captured checkpoint manifest repo mismatch
```

而且 manifest 指向的是：

```text
C:\Users\15126\Desktop\ruijie
```

不是实际测试仓库：

```text
C:\Users\15126\Desktop\ruijie\git-ai-view\git-ai-auto-upload-e2e
```

因此，这一轮结论只能用于说明“嵌套仓库会污染验证结果”，不能拿来判断自动上传功能本身是否成功。

### 10.3 第二次试跑：独立仓库 standalone

为了排除嵌套仓库干扰，重新在工作区外创建了独立仓库：

```text
C:\Users\15126\Desktop\git-ai-auto-upload-standalone
```

实测 commit 如下：

1. 预检 commit：`eff679084dd47b5df43ee233687bcb2a750bbcca`
2. 自动上传验证 commit：`339a9b4517739d4b8fdfec7e4a1be72e93ebd47c`

这轮测试的关键现象：

1. commit 当下会打印：

```text
[git-ai] still processing commit ...
```

2. 稍后回查时，AI note 已经最终生成。
3. `git-ai stats <sha> --json` 可以正常返回，统计值是：
    - `unknown_additions = 1`
    - `ai_additions = 0`
    - `human_additions = 0`
    - `git_diff_added_lines = 1`
4. 针对第二条验证 commit 执行只读 SQL：

```sql
SELECT COUNT(*)
FROM cr.git_ai_commit_stats
WHERE commit_code = '339a9b4517739d4b8fdfec7e4a1be72e93ebd47c';
```

结果是：

```text
0
```

结论：独立仓库已经排除了嵌套仓库干扰，但自动上传仍未落库。

### 10.4 第三次试跑：重启 `git-ai.exe bg run` 后的受控复测

为了专门排除“上传环境变量没有进入后台 daemon”这个假设，又做了一轮控制变量复测。

这轮复测的控制方式是：

1. 先停掉现有 `git-ai.exe bg run`。
2. 在专用终端里，以带环境变量的方式重新启动 daemon：

```powershell
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "1"
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
$env:GIT_AI_DEBUG = "1"
git-ai bg run
```

3. 在真正执行 commit 的 shell 里，明确移除这些环境变量，保证 commit shell 本身不携带上传配置，只依赖 daemon 自己继承到的环境变量。
4. 然后在全新的独立仓库中复测。

这轮受控复测使用的仓库是：

```text
C:\Users\15126\Desktop\git-ai-auto-upload-daemon-controlled
```

实测 commit 如下：

1. 预检 commit：`d8d05df89420cf1097e74955ae72b07fbec56614`
2. 自动上传验证 commit：`7f1e55014def5addcc14a28dcac7f3c274be16fc`

本地行为结果：

1. 第一条 commit 的 note 最终正常生成。
2. 第二条 commit 当下仍然打印：

```text
[git-ai] still processing commit 7f1e5501... run `git ai stats` to see stats.
```

3. 稍后回查时，第二条 commit 的 note 也最终生成。
4. `git-ai stats 7f1e55014def5addcc14a28dcac7f3c274be16fc --json` 返回的关键值仍然是：
    - `unknown_additions = 1`
    - `ai_additions = 0`
    - `human_additions = 0`
    - `git_diff_added_lines = 1`

数据库只读核验结果：

```sql
SELECT COUNT(*)
FROM cr.git_ai_commit_stats
WHERE commit_code = '7f1e55014def5addcc14a28dcac7f3c274be16fc';
```

结果仍然是：

```text
0
```

也就是说，即使先重启 `git-ai.exe bg run`，并确保 daemon 在启动时就携带了上传环境变量，第二条验证 commit 仍然没有落到数据库。

### 10.5 为什么可以认为“环境变量没进 daemon”不是主要原因

这不是主观判断，而是结合了“实测结果 + 源码路径”得到的结论。

1. 受控复测中，真正处理 commit 的 daemon 日志文件是：

```text
C:\Users\15126\.git-ai\internal\daemon\logs\14688.log
```

2. 这份日志里明确记录了受控仓库的两次 commit：
    - `d8d05df89420cf1097e74955ae72b07fbec56614`
    - `7f1e55014def5addcc14a28dcac7f3c274be16fc`
3. 这说明新 daemon 确实接管了这轮测试。
4. 代码实现上，[git-ai/src/commands/daemon.rs](../../../git-ai/src/commands/daemon.rs) 在 Windows 下拉起 detached daemon 时，只会移除 Git 定位相关变量和 `GIT_AI` 本身，不会主动移除 `GIT_AI_AUTO_UPLOAD_AI_STATS`、`GIT_AI_REPORT_REMOTE_URL`、`GIT_AI_DEBUG` 这类 `GIT_AI_*` 上传环境变量。
5. 同时，[git-ai/src/daemon.rs](../../../git-ai/src/daemon.rs) 里被清理的也是 `GIT_DIR`、`GIT_WORK_TREE` 这类 Git 运行时变量，而不是上传配置变量。

因此，本次受控复测后，可以把下面这个假设降级：

```text
“自动上传失败的主要原因是：环境变量没有进入后台 daemon”
```

当前证据更支持的是：

1. 本地异步 post-commit 处理是通的，至少 note/stats 最终能生成。
2. 自动上传没有形成数据库记录。
3. 失败原因更可能在上传触发时机、上传线程实际执行情况，或者远端鉴权 / 用户上下文这一层。

### 10.6 当前阶段的可确认结论

截至 2026-04-26，本次验证可以确认以下事实：

1. 已安装的 `git-ai 1.3.4` 在新仓库里能够正常 `git-ai install`。
2. commit 后 AI note 和 `git-ai stats` 会异步生成，不是完全失效。
3. 在 standalone 仓库中，自动上传验证 commit 没有落入 `cr.git_ai_commit_stats`。
4. 在“重启 daemon 且预置上传环境变量”的受控复测中，自动上传验证 commit 仍然没有落入 `cr.git_ai_commit_stats`。
5. 因此，“环境变量没进 daemon”已经不是当前问题的主要嫌疑项。

### 10.7 建议的下一步验证方向

如果后续继续查，不建议再重复做同样的 daemon 重启实验，而是直接转向下面两个方向：

1. 验证自动上传线程是否在当前 async / daemon 路径中真正触发到了 [git-ai/src/integration/upload_stats.rs](../../../git-ai/src/integration/upload_stats.rs)。
2. 验证在 `git-ai whoami = logged out`、且未显式设置 `GIT_AI_REPORT_REMOTE_API_KEY` / `GIT_AI_REPORT_REMOTE_USER_ID` 的情况下，远端接口是否会直接拒绝请求，且当前失败日志没有被收集出来。

### 10.8 为下一轮排查补充接口调用点日志

为了避免后续继续靠“数据库有没有落库”做黑盒猜测，已经在 [git-ai/src/integration/upload_stats.rs](../../../git-ai/src/integration/upload_stats.rs) 补了接口调用点的持久化日志。

这次补充的日志点包括：

1. `auto_upload_ai_stats` feature flag 未开启时，明确记录：

```text
feature flag auto_upload_ai_stats disabled; skipping upload for <sha>
```

2. 真正准备发请求时，明确记录：

```text
starting upload for <sha> url=<...> has_api_key=<true|false> has_user_id=<true|false>
```

3. 进入 HTTP 调用后，明确记录：

```text
perform_upload url=<...> has_api_key=<true|false> has_user_id=<true|false>
perform_upload response status=<status> url=<...>
```

4. 请求结束后，明确记录：

```text
uploaded stats for <sha>
```

或：

```text
upload failed for <sha>: ...
```

这些日志会同时写入 tracing 日志；如果设置了 `GIT_AI_DEBUG`，也会继续向 stderr 输出。

需要特别注意：这只是源码层改动，不会自动进入当前机器上已经安装好的旧二进制。也就是说：

1. 当时已安装的 `git-ai 1.3.4` 不会立刻出现这些新增日志。
2. 必须在后续重新 build / install 当前源码后，再做下一轮验证，才能利用这些日志判断到底是：
    - 根本没进入 `maybe_upload_after_commit`
    - feature flag 没打开
    - 线程已启动但请求未发出
    - 请求已发出但远端返回失败

### 10.9 源码版重装后的新日志复测

随后已经在本机补齐 Rust 工具链和 MSVC Build Tools，并把当前源码版重新安装到了本机。

这轮重装后的安装态快照是：

1. `git-ai --version = 1.3.5 (debug)`
2. 安装路径是：

```text
C:\Users\15126\.git-ai\bin\git-ai.exe
```

3. `git-ai config` 显示：
    - `feature_flags.async_mode = false`
    - `feature_flags.git_hooks_enabled = false`

这意味着：当前这份源码版验证时，主路径不是 async daemon，而是同步 wrapper 路径。也因此，单纯重启 `git-ai.exe bg run` 对这份 `1.3.5 (debug)` 安装态不是主要控制变量。

#### 10.9.1 第一轮：沿用旧文档写法，把开关设成 `"1"`

使用仓库：

```text
C:\Users\15126\Desktop\git-ai-auto-upload-source-shellenv
```

在 commit shell 中设置：

```powershell
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "1"
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
$env:GIT_AI_DEBUG = "1"
```

随后补打第三条验证 commit：

```text
f5c30dc14a4b4d7301ba6aba098852cf938928e4
```

终端实际打印出了新的源码日志：

```text
[git-ai] upload-ai-stats: feature flag auto_upload_ai_stats disabled; skipping upload for f5c30dc
```

这条日志的含义非常直接：

1. 新增日志已经真正进入当前安装二进制。
2. 旧文档里 `GIT_AI_AUTO_UPLOAD_AI_STATS="1"` 这个写法，对当前实现并不会打开 feature flag。

所以，后续所有验证都必须改用：

```powershell
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "true"
```

#### 10.9.2 第二轮：把开关改成 `"true"`

在同一个仓库中，把环境变量改成：

```powershell
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "true"
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
$env:GIT_AI_DEBUG = "1"
```

再打第四条验证 commit：

```text
d69880ef1c842a91b3b7f33eaf38e25d6e4346b2
```

本地结果：

1. 这次不再出现 `feature flag auto_upload_ai_stats disabled`。
2. 但也没有出现：
    - `starting upload for ...`
    - `perform_upload ...`
    - `uploaded stats for ...`
    - `upload failed for ...`
3. 本地 `git-ai stats d69880ef1c842a91b3b7f33eaf38e25d6e4346b2 --json` 仍然正常返回。
4. 数据库只读查询结果仍然是：

```sql
SELECT COUNT(*)
FROM cr.git_ai_commit_stats
WHERE commit_code = 'd69880ef1c842a91b3b7f33eaf38e25d6e4346b2';
```

结果：

```text
0
```

#### 10.9.3 这轮复测后的新判断

源码版重装 + 新日志复测后，当前可以确认两件事：

1. **第一个真实问题已经确认**：

```text
GIT_AI_AUTO_UPLOAD_AI_STATS="1" 不会打开当前实现里的 feature flag
```

必须改成：

```text
GIT_AI_AUTO_UPLOAD_AI_STATS="true"
```

2. **第二个更深层的问题也已经浮现**：

在 `1.3.5 (debug)` 安装态下，`auto_upload_ai_stats=true` 已经被 runtime 识别，但 commit 后仍然没有出现任何 `starting upload` / `perform_upload` / `upload failed` / `uploaded stats` 日志，数据库也没有记录。

结合源码路径，当前最强嫌疑是：

1. 当前安装态走的是同步 wrapper 路径，而不是 async daemon 路径。
2. 上传逻辑在 [git-ai/src/integration/upload_stats.rs](../../../git-ai/src/integration/upload_stats.rs) 里使用了：

```rust
std::thread::spawn(...)
```

3. 但 wrapper 路径在 [git-ai/src/commands/git_handlers.rs](../../../git-ai/src/commands/git_handlers.rs) 里会很快执行：

```rust
exit_with_status(...)
```

4. 因此，上传线程很可能在真正发出请求前就随着宿主进程退出而被终止。

这个结论还不是最终定论，但已经比“网络不通”“接口拒绝”“daemon 没吃到环境变量”更接近根因。

### 10.10 重启 VS 后的无代码改动复测

在没有继续修改源码的前提下，又做了一轮“重启 VS 后立即复测”，目的是确认 VS 重启是否会让当前安装态的运行路径或上传结果发生变化。

这轮复测使用的仍然是已经重装到本机的源码版：

1. `git-ai --version = 1.3.5 (debug)`
2. 可执行路径：

```text
C:\Users\15126\.git-ai\bin\git-ai.exe
```

3. 在无额外环境变量时，`git-ai config` 看到的关键运行态是：
    - `feature_flags.auto_upload_ai_stats = false`
    - `feature_flags.async_mode = false`
    - `feature_flags.git_hooks_enabled = false`

#### 10.10.1 复测仓库与运行方式

使用了一个新的独立仓库：

```text
C:\Users\15126\Desktop\git-ai-auto-upload-after-vs-restart
```

这轮复测中：

1. 不做任何源码修改。
2. 不做任何 push。
3. 在 commit 所在 shell 中显式设置：

```powershell
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "true"
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
$env:GIT_AI_DEBUG = "1"
```

4. 然后再次通过 `git-ai config` 确认：

```text
feature_flags.auto_upload_ai_stats = true
```

也就是说，这轮复测里 feature flag 已经被 runtime 正确识别为开启状态。

#### 10.10.2 实测 commit 结果

本次复测共有两次 commit：

1. 预检 commit：`51c4568...`
2. 验证 commit：`17b53acc4ba600d9585c95eb02857127ecb7c778`

第二条验证 commit 在终端里的表现是：

1. 正常 commit 输出存在。
2. 正常 attribution bar 存在。
3. **没有出现任何** `[git-ai] upload-ai-stats:` 相关日志。

也就是说，这一轮在以下日志点上仍然完全空白：

```text
starting upload for ...
perform_upload ...
upload failed for ...
uploaded stats for ...
```

#### 10.10.3 本地 note / stats 结果

尽管没有出现 upload 相关日志，但本地 note 和 stats 仍然正常生成：

1. `git notes --ref=ai list` 能看到第二条验证 commit 的 note。
2. `git-ai stats 17b53acc4ba600d9585c95eb02857127ecb7c778 --json` 正常返回。
3. 关键统计仍然是：
    - `git_diff_added_lines = 1`
    - `unknown_additions = 1`
    - `ai_additions = 0`

#### 10.10.4 数据库只读核验结果

对第二条验证 commit 执行只读 SQL：

```sql
SELECT COUNT(*)
FROM cr.git_ai_commit_stats
WHERE commit_code = '17b53acc4ba600d9585c95eb02857127ecb7c778';
```

结果仍然是：

```text
0
```

因此，这一轮复测中，数据库仍然没有接收到该 commit 的上传结果。

#### 10.10.5 这轮复测的结论

重启 VS 之后，结果**没有实质变化**：

1. `GIT_AI_AUTO_UPLOAD_AI_STATS=true` 已经被 runtime 正确识别。
2. 本地 note / stats 仍然正常生成。
3. commit 时仍然没有任何 upload 阶段日志。
4. 数据库中仍然没有对应 `commit_code` 记录。

所以，当前可以进一步排除下面这个方向：

```text
“问题只是 VS 里某个旧进程或旧状态没有刷新”
```

截至目前，重启 VS 与否，都没有改变最终现象。

### 10.11 修复上传线程生命周期后的成功复测

在确认 sync wrapper 路径下 `post_commit -> maybe_upload_after_commit` 已经真实执行，但上传逻辑原先通过 `std::thread::spawn(...)` fire-and-forget 发起之后，又对 `git-ai/src/integration/upload_stats.rs` 做了一个最小修复：

1. `async_mode=true` 的 daemon 场景仍然保留后台上传。
2. `async_mode=false` 的 sync wrapper 场景改为当前线程直接执行上传。

这个修复的目的很明确：

```text
避免 wrapper 在 post-commit 结束后立即退出，把刚创建的上传线程一并带死
```

修复后先跑了窄测试：

```text
cargo test --lib run_upload_task_ -- --nocapture
```

结果：

```text
2 passed; 0 failed
```

然后重新安装当前源码版，再做一轮真正的端到端验证。

#### 10.11.1 重新安装结果

使用了仓库自带安装脚本：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\dev.ps1
```

安装成功后，当前本机二进制仍然是：

```text
git-ai 1.3.5 (debug)
C:\Users\15126\.git-ai\bin\git-ai.exe
```

#### 10.11.2 成功复测仓库与 commit

本轮使用新的独立仓库：

```text
C:\Users\15126\Desktop\git-ai-auto-upload-inline-fix
```

验证时明确使用安装后的 shim：

```text
C:\Users\15126\.git-ai\bin\git.exe
```

并在 commit 所在 shell 中设置：

```powershell
$env:GIT_AI_AUTO_UPLOAD_AI_STATS = "true"
$env:GIT_AI_REPORT_REMOTE_URL = "https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats"
$env:GIT_AI_DEBUG = "1"
```

本轮验证 commit 的完整 SHA 是：

```text
966640be98e82a8109629ba63b4bed52f97f4787
```

#### 10.11.3 commit 时的上传日志证据

这一次，commit 终端里已经出现了完整上传日志，关键证据如下：

```text
[git-ai] upload-ai-stats: starting upload for 966640b mode=inline ...
[git-ai] upload-ai-stats: perform_upload url=https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats ...
[git-ai] upload-ai-stats: perform_upload response status=200 ...
[git-ai] upload-ai-stats: uploaded stats for 966640b
```

这说明至少以下几点已经同时成立：

1. sync wrapper 路径已经真正进入上传逻辑。
2. 上传请求已经真实发出。
3. 服务端已经返回 HTTP 200。
4. 上传动作在当前进程退出前已经完成。

其中最关键的新信号是：

```text
mode=inline
```

这与修复目标完全一致，说明当前不是再把上传工作丢给一个即将随 wrapper 退出的后台线程。

#### 10.11.4 本地 note / stats 结果

修复后，本地归因产物仍然正常：

1. `git notes --ref=ai list` 中能看到 `966640be98e82a8109629ba63b4bed52f97f4787` 的 note。
2. `git-ai stats 966640be98e82a8109629ba63b4bed52f97f4787 --json` 正常返回。
3. 本次验证 commit 的核心统计是：
    - `git_diff_added_lines = 1`
    - `unknown_additions = 1`
    - `ai_additions = 0`

也就是说，修复并没有破坏本地 note / stats 生成链路。

#### 10.11.5 数据库只读核验结果

对这条验证 commit 做只读 SQL：

```sql
SELECT COUNT(*) AS commit_row_count
FROM cr.git_ai_commit_stats
WHERE commit_code = '966640be98e82a8109629ba63b4bed52f97f4787';
```

结果是：

```text
1
```

这说明该条 commit 已经成功落入：

```text
cr.git_ai_commit_stats
```

#### 10.11.6 这轮成功复测的结论

到这里可以把本次问题的根因和修复效果基本坐实：

1. 之前失败时，真正的问题不是数据库、不是接口地址、也不是 VS / daemon 是否重启。
2. 真正问题在于 sync wrapper 场景下，上传工作被放进新线程后，宿主进程过快退出，导致上传来不及执行完成。
3. 改为 sync wrapper inline 上传后，commit 期间可以看到完整 upload 日志。
4. 服务端返回 200，数据库也能按 `commit_code` 查到记录。

因此，当前这条端到端链路已经验证通过：

```text
commit -> 本地 note/stats -> 自动上传 -> 服务端 200 -> cr.git_ai_commit_stats 落库
```

#### 10.11.7 当前默认配置状态

在完成上面的上传生命周期修复之后，又进一步把 `git-ai/src/feature_flags.rs` 里的：

```text
auto_upload_ai_stats
```

默认值从关闭改成了开启，并重新安装到本机验证。

在**不设置**下面这些环境变量的前提下：

```text
GIT_AI_AUTO_UPLOAD_AI_STATS
GIT_AI_REPORT_REMOTE_URL
GIT_AI_REPORT_REMOTE_ENDPOINT
GIT_AI_REPORT_REMOTE_PATH
GIT_AI_DEBUG
```

直接执行：

```powershell
C:\Users\15126\.git-ai\bin\git-ai.exe config
```

当前安装态看到的关键结果是：

```text
auto_upload_ai_stats = true
async_mode = false
git_hooks_enabled = false
```

这意味着对当前这版代码来说：

1. 正常安装用户**不再需要**手工设置 `GIT_AI_AUTO_UPLOAD_AI_STATS=true`。
2. 上传 URL 也本来就有内置默认值，除非要切到别的服务地址，否则也不需要额外设置。
3. 相关环境变量现在主要只用于：
    - 临时关闭上传
    - 覆盖上传地址
    - 打开调试日志

#### 10.11.8 prompt 明细上传专项验证（2026-04-26）

为了单独验证 `prompts[]` 是否真的能沿着“本地 note -> 上传脚本 -> 已部署服务 -> 数据库”这条链路落库，本轮重新创建了一个本地临时仓库：

```text
C:\Users\15126\Desktop\git-ai-prompt-upload-validation
```

仓库使用安装态 git-ai shim 初始化并提交了一次新的本地 commit，提交 SHA 为：

```text
23fe87862cd13377246a73da4b6e60eae322a3db
```

#### 10.11.8.1 本地 note / stats 预检查

commit 时显式设置：

```text
GIT_AI_AUTO_UPLOAD_AI_STATS=false
```

目的是先只验证本地 `authorship note` 和 `stats`，避免 auto upload 先一步把结果发出去。

本地验证结果如下：

1. `git notes --ref=ai list 23fe87862cd13377246a73da4b6e60eae322a3db` 能查到 note。
2. `git notes --ref=ai show 23fe87862cd13377246a73da4b6e60eae322a3db` 的 JSON metadata 段中存在**非空** `prompts` 对象。
3. note 中能看到类似下面的 prompt 元数据片段：

```json
"prompts": {
    "12d50c853b194322": {
        "agent_id": {
            "tool": "github-copilot",
            "model": "copilot/gpt-5.4"
        }
    }
}
```

4. `git-ai stats 23fe87862cd13377246a73da4b6e60eae322a3db --json` 的关键统计是：
        - `ai_additions = 63`
        - `human_additions = 0`
        - `unknown_additions = 0`
        - `tool_model_breakdown` 非空

这说明这条本地 commit 本身就是一个有效的 prompt 上传验证样本，而不是只有 `unknown_additions` 的空壳 commit。

#### 10.11.8.2 按文档路径执行手动上传

随后在该本地仓库目录中，直接调用 Spec Kit 的 PowerShell 上传脚本：

```powershell
powershell -ExecutionPolicy Bypass -File \
    C:\Users\15126\Desktop\ruijie\git-ai-view\spec-kit\scripts\powershell\upload-ai-stats.ps1 \
    -Commits "23fe87862cd13377246a73da4b6e60eae322a3db" \
    -LogHttpPayload
```

上传阶段看到的关键证据是：

1. 脚本执行成功，返回单 commit `uploaded`。
2. HTTP 请求体中，这条 commit 顶层确实带出了**非空** `prompts[]`。
3. 请求体里能看到类似下面的 prompt 上传项：

```json
{
    "promptHash": "12d50c853b194322",
    "tool": "github-copilot",
    "model": "copilot/gpt-5.4"
}
```

也就是说，客户端上传链路现在已经不是“只有 commit 统计，没有 prompt 明细”了；`prompts[]` 的确发出去了。

#### 10.11.8.3 数据库只读核验结果

对同一条 commit 做数据库只读查询：

```sql
SELECT id, summary_id, commit_code, author, create_time, total_add_line, ai_add_line, manual_add_line
FROM cr.git_ai_commit_stats
WHERE commit_code = '23fe87862cd13377246a73da4b6e60eae322a3db';

SELECT p.id, p.prompt_hash, p.tool, p.model, p.accepted_lines, p.total_additions, p.total_deletions, p.overriden_lines
FROM cr.git_ai_prompt_stats p
JOIN cr.git_ai_commit_stats c ON p.commit_id = c.id
WHERE c.commit_code = '23fe87862cd13377246a73da4b6e60eae322a3db';

SELECT t.id, t.tool, t.model, t.add_line, t.source_type
FROM cr.git_ai_tool_stats t
JOIN cr.git_ai_commit_stats c ON t.source_id = c.id AND t.source_type = 0
WHERE c.commit_code = '23fe87862cd13377246a73da4b6e60eae322a3db';
```

实际结果如下：

1. `git_ai_commit_stats` 已经存在这条新 commit：
        - `commit_code = 23fe87862cd13377246a73da4b6e60eae322a3db`
        - `total_add_line = 63`
        - `ai_add_line = 63`
        - `manual_add_line = 0`
2. `git_ai_prompt_stats` 已经存在 1 条 prompt 记录：
        - `prompt_hash = 12d50c853b194322`
        - `tool = github-copilot`
        - `model = copilot/gpt-5.4`
        - `accepted_lines = 63`
3. `git_ai_tool_stats` 的 commit 级工具统计也已经存在 1 条记录：
        - `tool = github-copilot`
        - `model = copilot/gpt-5.4`
        - `add_line = 63`

#### 10.11.8.4 本轮结论

这轮专项验证可以拆成两个结论：

1. **prompt 明细上传能力已经验证通过。**
     也就是：

```text
本地 note 含 prompts -> upload-ai-stats.ps1 请求体含 prompts[] -> git_ai_prompt_stats 成功落库
```

2. **`tool/model` 归一化在 prompt 场景下仍然没有完全修干净。**
     本次新样本落库后，`git_ai_prompt_stats.model` 和 `git_ai_tool_stats.model` 仍然都是：

```text
copilot/gpt-5.4
```

而不是期望中的：

```text
gpt-5.4
```

因此，本轮不能下“prompt 上传和 tool/model 分列都完全通过”的结论。更准确的说法应该是：

```text
prompt 明细上传已通过；
但 prompt / commit tool 记录里的 model 字段仍残留 tool/model 组合值，归一化还需继续修复。
```

#### 10.11.9 prompt/tool 归一化修复后复测（2026-04-26）

针对 10.11.8 暴露出来的这一类组合值：

```text
tool = github-copilot
model = copilot/gpt-5.4
```

本轮先修了上传脚本在 prompt/tool 归一化时的比较逻辑，使它在 `github-copilot` 与 `copilot` 属于同一工具族时，也能把：

```text
copilot/gpt-5.4
```

规整为：

```text
gpt-5.4
```

#### 10.11.9.1 上传脚本请求体验证

修复后，继续复用上一轮的本地验证方式，新建第二个本地临时仓库：

```text
C:\Users\15126\Desktop\git-ai-prompt-upload-validation-v2
```

在该仓库中创建新的本地 commit：

```text
02c2f9c31a84db5086ae57255ddd734827f0e765
```

这条 commit 的本地 note / stats 预检查结果为：

1. `git notes --ref=ai show 02c2f9c31a84db5086ae57255ddd734827f0e765` 中 `prompts` 非空。
2. `git-ai stats 02c2f9c31a84db5086ae57255ddd734827f0e765 --json` 中：
        - `ai_additions = 54`
        - `tool_model_breakdown` 非空

随后再次调用上传脚本，并打开请求体日志。修复后的关键证据是：

```json
{
    "promptHash": "12d50c853b194322",
    "tool": "github-copilot",
    "model": "gpt-5.4"
}
```

以及 commit 级 `toolModelBreakdown` 也已经变成：

```json
{
    "tool": "github-copilot",
    "model": "gpt-5.4",
    "aiAdditions": 54,
    "aiAccepted": 54
}
```

这说明客户端上传链路里，`copilot/gpt-5.4` 已经在发请求之前被规整成了 `gpt-5.4`。

#### 10.11.9.2 数据库只读复测结果

对这条新的验证 commit 执行数据库只读查询：

```sql
SELECT p.id, p.prompt_hash, p.tool, p.model, p.accepted_lines
FROM cr.git_ai_prompt_stats p
JOIN cr.git_ai_commit_stats c ON p.commit_id = c.id
WHERE c.commit_code = '02c2f9c31a84db5086ae57255ddd734827f0e765';

SELECT t.id, t.tool, t.model, t.add_line
FROM cr.git_ai_tool_stats t
JOIN cr.git_ai_commit_stats c ON t.source_id = c.id AND t.source_type = 0
WHERE c.commit_code = '02c2f9c31a84db5086ae57255ddd734827f0e765';

SELECT
    SUM(CASE WHEN p.model LIKE '%/%' OR p.model LIKE '%::%' THEN 1 ELSE 0 END) AS dirty_prompt_models,
    COUNT(*) AS total_prompt_rows
FROM cr.git_ai_prompt_stats p
JOIN cr.git_ai_commit_stats c ON p.commit_id = c.id
WHERE c.commit_code = '02c2f9c31a84db5086ae57255ddd734827f0e765';

SELECT
    SUM(CASE WHEN t.model LIKE '%/%' OR t.model LIKE '%::%' THEN 1 ELSE 0 END) AS dirty_commit_tool_models,
    COUNT(*) AS total_commit_tool_rows
FROM cr.git_ai_tool_stats t
JOIN cr.git_ai_commit_stats c ON t.source_id = c.id AND t.source_type = 0
WHERE c.commit_code = '02c2f9c31a84db5086ae57255ddd734827f0e765';
```

实际结果：

1. `git_ai_prompt_stats`：
        - `tool = github-copilot`
        - `model = gpt-5.4`
        - `accepted_lines = 54`
2. `git_ai_tool_stats`（commit 级）：
        - `tool = github-copilot`
        - `model = gpt-5.4`
        - `add_line = 54`
3. 脏值检查：
        - `dirty_prompt_models = 0`
        - `dirty_commit_tool_models = 0`

#### 10.11.9.3 本轮结论

这次复测可以下明确结论：

```text
在当前“本地新建 prompt commit -> upload-ai-stats.ps1 手动上传 -> 已部署服务落库”这条验证路径上，
model 字段已经成功从 copilot/gpt-5.4 规整为 gpt-5.4。
```

也就是说，针对本轮最核心的验收目标：

```text
数据库里的 model 只剩 gpt-5.4
```

这条结论现在已经验证通过。

#### 10.11.10 git-ai 原生 auto upload 归一化复测（2026-04-26）

在 10.11.9 中，已经确认 PowerShell 手动上传路径不会再把：

```text
tool = github-copilot
model = copilot/gpt-5.4
```

打回数据库。

但这还不够，因为真实用户默认走的是 `git-ai` 原生自动上传链路；如果 Rust 侧仍然保留旧逻辑，那么后续 auto upload 仍然可能把脏值重新写回库里。

因此，本轮专门针对“本地 commit -> git-ai 原生 auto upload -> 已部署服务落库”这条链路做复测。

#### 10.11.10.1 本地 auto upload 日志验证

本轮新建独立临时仓库：

```text
C:\Users\15126\Desktop\git-ai-auto-upload-validation-v3
```

第一次提交：

```text
010ac512c46a31f11b0d7f278d54a0c0a942bf4f
```

该次提交能看到 `starting upload` 起始日志，且本地 `git notes --ref=ai show` 中 `prompts` 非空，但数据库中没有对应行，因此不能作为通过样本。

随后在同一仓库继续做第二次最小变更提交：

```text
1e37b12aab159fd9975dd3e9e6367cb4bb87890a
```

这次 commit 终端日志完整出现：

```text
[git-ai] upload-ai-stats: starting upload for 1e37b12 mode=inline ...
[git-ai] upload-ai-stats: perform_upload response status=200 ...
[git-ai] upload-ai-stats: uploaded stats for 1e37b12
```

这说明当前已安装的 `git-ai`，在 sync-wrapper / inline 上传路径下，已经真正把请求发到服务端并拿到 `200` 响应。

#### 10.11.10.2 数据库只读核验结果

针对这条成功样本 commit，执行数据库只读查询：

```sql
SELECT id, summary_id, commit_code, author, total_add_line, ai_add_line, manual_add_line
FROM cr.git_ai_commit_stats
WHERE commit_code = '1e37b12aab159fd9975dd3e9e6367cb4bb87890a';

SELECT p.id, p.prompt_hash, p.tool, p.model, p.accepted_lines, p.total_additions, p.total_deletions,
       p.overriden_lines, p.prompt_text IS NOT NULL AS has_prompt_text
FROM cr.git_ai_prompt_stats p
JOIN cr.git_ai_commit_stats c ON p.commit_id = c.id
WHERE c.commit_code = '1e37b12aab159fd9975dd3e9e6367cb4bb87890a';

SELECT t.id, t.tool, t.model, t.add_line
FROM cr.git_ai_tool_stats t
JOIN cr.git_ai_commit_stats c ON t.source_id = c.id AND t.source_type = 0
WHERE c.commit_code = '1e37b12aab159fd9975dd3e9e6367cb4bb87890a';

SELECT
    COALESCE(SUM(CASE WHEN p.model LIKE '%/%' OR p.model LIKE '%::%' THEN 1 ELSE 0 END), 0) AS dirty_prompt_models,
    COUNT(*) AS total_prompt_rows
FROM cr.git_ai_prompt_stats p
JOIN cr.git_ai_commit_stats c ON p.commit_id = c.id
WHERE c.commit_code = '1e37b12aab159fd9975dd3e9e6367cb4bb87890a';

SELECT
    COALESCE(SUM(CASE WHEN t.model LIKE '%/%' OR t.model LIKE '%::%' THEN 1 ELSE 0 END), 0) AS dirty_commit_tool_models,
    COUNT(*) AS total_commit_tool_rows
FROM cr.git_ai_tool_stats t
JOIN cr.git_ai_commit_stats c ON t.source_id = c.id AND t.source_type = 0
WHERE c.commit_code = '1e37b12aab159fd9975dd3e9e6367cb4bb87890a';
```

实际结果：

1. `git_ai_commit_stats`：
    - 存在 commit 主记录
    - `author = auto-upload-validation@example.com`
    - `total_add_line = 1`
    - `ai_add_line = 1`
    - `manual_add_line = 0`
2. `git_ai_prompt_stats`：
    - `tool = github-copilot`
    - `model = gpt-5.4`
    - `accepted_lines = 42`
    - `has_prompt_text = 0`
3. `git_ai_tool_stats`（commit 级）：
    - `tool = github-copilot`
    - `model = gpt-5.4`
    - `add_line = 1`
4. 脏值检查：
    - `dirty_prompt_models = 0`
    - `dirty_commit_tool_models = 0`

#### 10.11.10.3 关于 prompt_text 为空的说明

本轮数据库里 `prompt_text` 仍然为空，但这不是“因为重新测试了一次”导致的，也不是上传脚本把字段弄丢了。

当前已确认的根因是：

1. `prompt_text` 来源于 note 中的 `prompts[].messages`。
2. 本次验证样本里，本地 note 已经是 `messages: []`，上传端没有可提取的具体提示词文本。
3. 同时，`git-ai` 在 release 构建下会通过 `serialize_messages_release_empty` 主动把 prompt messages 序列化为空数组。

所以，现状更准确的结论是：

```text
prompt 明细记录已经能上传并落库；
但如果 note 中没有保留 messages，服务端就无法反推出 prompt_text。
```

如果后续业务要求数据库里必须看到具体提示词内容，这会是另一条独立需求，需要在 `git-ai` 侧保留 messages，或者增加 `messages_url/CAS` 回查能力，而不是继续改当前上传归一化逻辑。

#### 10.11.10.4 本轮结论

这次复测可以下明确结论：

```text
在当前“本地新建 prompt commit -> git-ai 原生 auto upload -> 已部署服务落库”这条真实链路上，
prompt / commit tool 的 model 字段都已经稳定落为 gpt-5.4，
不会再被回写成 copilot/gpt-5.4 这类 tool/model 组合值。
```

也就是说，本轮用户关心的第二个核心验收点：

```text
auto upload 以后不会再把 copilot/gpt-5.4 打回库里
```

现在已经通过真实数据库验证。