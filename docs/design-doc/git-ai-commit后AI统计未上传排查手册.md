# git-ai commit 后 AI 统计未上传排查手册

## 1. 适用场景

这份文档用于排查下面这类问题：

1. 开发者已经执行了 `git commit`，本地提交成功。
2. 代码仓库里预期应该由 `git-ai` 自动上传 AI 统计。
3. 远端平台、数据库或看板里没有看到这次 commit 的统计结果。

当前实现下，这类问题不能直接归因成“上传接口异常”。真正的链路是：

```text
git commit
  -> post-commit hook
  -> 写入 authorship note
  -> 计算 commit stats
  -> 尝试自动上传
  -> 服务端接收并落库
```

只要上面任何一层没走通，最终表现都会是“commit 后没有上传”。

如果问题不是“上传没发生”，而是“AI 生成的代码本身没有被正确识别成 AI”，请同时参考：

1. [docs/review-results/git-ai-AI生成代码未被正确识别排查手册.md](docs/review-results/git-ai-AI生成代码未被正确识别排查手册.md)

---

## 2. 先记住 4 个结论

1. 自动上传依赖 `post-commit` hook 真正执行，不是只要安装了 `git-ai` 就一定会传。
2. 当前版本里 `auto_upload_ai_stats` 默认开启；只有显式配置成 `false` 才会关闭。
3. merge commit 和“过大提交”会跳过 post-commit 的 stats 计算；一旦本次没有算出 `stats`，自动上传也会一起跳过。
4. 自动上传是 best-effort。即使 HTTP 失败，`git commit` 仍然会成功，所以不能用“commit 没报错”反推“上传成功”。

代码依据：

1. `post-commit` 里只有在 `stats` 存在时才会调用上传逻辑，见 `git-ai/src/authorship/post_commit.rs`。
2. `auto_upload_ai_stats` 的默认值为 `true`，见 `git-ai/src/feature_flags.rs`。
3. 上传失败只打 debug 日志、不阻断 commit，见 `git-ai/src/integration/upload_stats.rs`。

---

## 3. 最短排查路径

建议先在出问题的仓库里，针对刚提交的那条 commit，按顺序执行下面命令：

```powershell
git notes --ref=ai list HEAD
git notes --ref=ai show HEAD
git-ai stats HEAD --json
git-ai debug
git config --show-origin --get core.hooksPath
```

如果需要继续验证上传环节，再执行：

```powershell
$env:GIT_AI_DEBUG = "1"
$sha = git rev-parse HEAD
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits $sha -LogHttpPayload
```

判定方法：

1. `git notes --ref=ai list HEAD` 没输出：优先查 hook。
2. `git notes` 有输出，但 `git-ai stats HEAD --json` 失败：优先查本地 stats。
3. `git-ai stats HEAD --json` 正常，但补传脚本失败：优先查远端配置、认证或网络。
4. 补传脚本成功，但 commit 后自动上传仍然没生效：优先查 `post-commit` 是否真的执行，以及本次 commit 是否被跳过 stats 计算。

---

## 4. 按故障层分段排查

### 4.1 第一层：`post-commit` hook 根本没跑

最直接的特征是：

```powershell
git notes --ref=ai list HEAD
```

没有任何输出。

这通常说明本次 commit 后根本没有写入 `refs/notes/ai`，问题还没进入上传阶段。

优先检查：

```powershell
git-ai debug
git config --show-origin --get core.hooksPath
git-ai install-hooks
```

重点看：

1. `git-ai debug` 输出里是否存在 `core.hooksPath`。
2. 当前仓库或全局 `core.hooksPath` 是否被其他工具接管。
3. 重新执行 `git-ai install-hooks` 后，再做一次最小 commit，note 是否恢复生成。

常见根因：

1. 仓库使用了自定义 `core.hooksPath`，但 `git-ai` hook 没装到真正生效的目录。
2. 其他工具覆盖了 hook 目录，导致 `post-commit` 没有调用 `git-ai`。
3. 用户只安装了 `git-ai`，但没有在当前仓库重新安装 hooks。

---

### 4.2 第二层：hook 跑了，但本地 `stats` 没有生成

最常见特征是：

```powershell
git notes --ref=ai show HEAD
```

可以看到 authorship note，但：

```powershell
git-ai stats HEAD --json
```

没有得到预期结果，或者自动上传完全没有发生。

当前实现里，下面两类 commit 会让 post-commit 跳过 stats 计算：

1. merge commit
2. 被判定为“过大”的 commit

也就是说，`note` 的存在只能证明 authorship 记录已经落地，不能证明本次 post-commit 一定算出了可上传的 `stats`。

建议这样验证：

```powershell
git rev-parse HEAD
git-ai stats HEAD --json
```

如果手动执行 `git-ai stats HEAD --json` 正常，通常说明：

1. 这条 commit 的统计并不是不可计算。
2. 只是自动上传所走的 post-commit 快路径在当时跳过了 stats。

这类场景下，最实用的处置方式通常不是继续盯着自动上传，而是直接补传这条 commit。

---

### 4.3 第三层：本地 `stats` 正常，但自动上传被关闭或未触发

先看环境变量：

```powershell
Get-ChildItem Env:GIT_AI_AUTO_UPLOAD_AI_STATS
Get-ChildItem Env:GIT_AI_DEBUG
```

注意：

1. 当前版本默认自动上传已经开启，正常情况下不需要再显式设置 `GIT_AI_AUTO_UPLOAD_AI_STATS=true`。
2. 如果有人本机或脚本里把它设成了 `false`，自动上传会被直接关闭。
3. 没有 `GIT_AI_DEBUG=1` 时，上传失败往往只表现为“远端没数据”，本地终端不一定有明显提示。

建议复测时这样做：

```powershell
$env:GIT_AI_DEBUG = "1"
git add .
git commit -m "test: verify auto upload"
```

然后看终端是否出现类似日志：

```text
[git-ai] upload-ai-stats: starting upload for <short_sha> ...
[git-ai] upload-ai-stats: uploaded stats for <short_sha>
```

判定方法：

1. 完全没有任何 `upload-ai-stats` 日志：说明自动上传调用大概率没有触发。
2. 有 `starting upload`，但没有成功日志：说明已经进入 HTTP 上传阶段，继续查远端。
3. 出现 `feature flag auto_upload_ai_stats disabled`：说明自动上传被明确关掉了。

---

### 4.4 第四层：上传调用发生了，但远端失败

这时重点已经不是本地 note/stats，而是远端请求参数是否完整。

建议检查这些变量：

```powershell
Get-ChildItem Env:GIT_AI_REPORT_REMOTE_URL
Get-ChildItem Env:GIT_AI_REPORT_REMOTE_ENDPOINT
Get-ChildItem Env:GIT_AI_REPORT_REMOTE_PATH
Get-ChildItem Env:GIT_AI_REPORT_REMOTE_USER_ID
Get-ChildItem Env:GIT_AI_REPORT_REMOTE_API_KEY
```

重点关注：

1. `GIT_AI_REPORT_REMOTE_URL` 是否被错误覆盖到了测试地址、旧地址或空值。
2. `GIT_AI_REPORT_REMOTE_USER_ID` 是否存在；它缺失时，服务端通常无法识别用户。
3. `GIT_AI_REPORT_REMOTE_API_KEY` 是否符合当前环境要求。
4. 本机网络是否可以访问远端网关。

如果要把请求体也一起打印出来，用：

```powershell
$sha = git rev-parse HEAD
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits $sha -LogHttpPayload
```

这个命令适合回答两个问题：

1. 本地到底有没有把这条 commit 的统计组装成请求。
2. 当前请求 URL、headers、payload 是否符合预期。

---

## 5. 最常见的判断矩阵

| 现场现象 | 最可能的问题层 | 先做什么 |
| --- | --- | --- |
| `git commit` 成功，但 `git notes --ref=ai list HEAD` 没输出 | hook 未生效 | 查 `core.hooksPath`，重跑 `git-ai install-hooks` |
| `git notes` 有内容，但看板没有数据 | stats 被跳过，或上传失败 | 先跑 `git-ai stats HEAD --json` |
| `git-ai stats HEAD --json` 正常，但自动上传没痕迹 | 自动上传未触发或被关闭 | 开 `GIT_AI_DEBUG=1` 再做一次小 commit |
| 有 `starting upload`，没有 `uploaded stats for` | HTTP 请求失败 | 查 URL、X-USER-ID、API Key、网络 |
| 手动补传成功，自动上传失败 | 自动上传链路问题，不是远端接口完全不可用 | 回查 `post-commit` 和 feature flag |

---

## 6. 最实用的补救方式：直接补传

如果当前目标是“先把缺失数据补上”，优先使用补传脚本，不要卡在一次自动上传复现上。

单条 commit 补传：

```powershell
$sha = git rev-parse HEAD
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits $sha
```

多条 commit 补传：

```powershell
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits "abc123,def456"
```

按时间范围补传：

```powershell
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Since "2026-04-01" -Until "2026-04-14"
```

只预览、不真正上传：

```powershell
.\.specify\scripts\powershell\upload-ai-stats.ps1 -DryRun
```

---

## 7. 建议让现场同事回传的证据

为了避免反复追问，建议一次性收集下面这些输出：

```powershell
git rev-parse HEAD
git notes --ref=ai list HEAD
git notes --ref=ai show HEAD
git-ai stats HEAD --json
git-ai debug
git config --show-origin --get core.hooksPath
Get-ChildItem Env:GIT_AI_REPORT_REMOTE_URL
Get-ChildItem Env:GIT_AI_REPORT_REMOTE_USER_ID
```

如果允许再次复测，再补：

```powershell
$env:GIT_AI_DEBUG = "1"
git add .
git commit -m "test: verify auto upload"
```

以及补传结果：

```powershell
$sha = git rev-parse HEAD
.\.specify\scripts\powershell\upload-ai-stats.ps1 -Commits $sha -LogHttpPayload
```

只要有了这些证据，基本就能快速判断问题是停在：

1. hook
2. 本地 note
3. 本地 stats
4. 自动上传开关
5. 远端 HTTP / 认证

---

## 8. 代码定位

如果后续需要继续追代码，优先看下面几个文件：

1. `git-ai/src/authorship/post_commit.rs`
2. `git-ai/src/integration/upload_stats.rs`
3. `git-ai/src/feature_flags.rs`
4. `spec-kit-standalone/spec-kit/scripts/powershell/upload-ai-stats.ps1`

这些文件分别对应：

1. commit 后 note 与 stats 的生成逻辑
2. 自动上传的触发、URL 解析、用户身份和 HTTP 上传逻辑
3. 自动上传 feature flag 的默认值与环境变量覆盖逻辑
4. 手动补传与请求体打印逻辑