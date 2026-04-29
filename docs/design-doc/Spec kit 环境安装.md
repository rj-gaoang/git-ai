# Spec kit 环境安装

|执行耗时|10分钟|
|---|---|
## 安装uvx

### Mac OS

```Bash

curl -LsSf https://astral.sh/uv/install.sh | sh
```

### Windows

1. 管理员模式打开PowerShell

2. 执行安装命令：

    ```PowerShell
    
    powershell -ExecutionPolicy ByPass -c "irm https://astral.sh/uv/install.ps1 | iex"
    ```

3. 关闭当前PowerShell窗口再重新打开，执行`uv`命令有命令列表代表安装成功

安装成功输出示例：

```Plain Text

PS C:\Windows\system32> uv
An extremely fast Python package manager.

Usage: uv.exe [OPTIONS] <COMMAND>

Commands:
  auth     Manage authentication
  run      Run a command or script
  init     Create a new project
  add      Add dependencies to the project
  remove   Remove dependencies from the project
  version  Read or update the project's version
  sync     Update the project's environment
  lock     Update the project's lockfile
  export   Export the project's lockfile to an alternate format
  tree     Display the project's dependency tree
  format   Format Python code in the project
  tool     Run and install commands provided by Python packages
  python   Manage Python versions and installations
  pip      Manage Python packages with a pip-compatible interface
  venv     Create a virtual environment
  build    Build Python packages into source distributions and wheels
  publish  Upload distributions to an index
  cache    Manage uv's cache
  self     Manage the uv executable
  help     Display documentation for a command
```

### 手动安装（解决阻塞卡顿问题）

#### 1. 在线安装UVX

浏览器打开 GitHub Release 页：[https://github.com/astral-sh/uv/releases](https://github.com/astral-sh/uv/releases)

找到最新版 `uv-x86_64-pc-windows-msvc.zip`（一般 10 MB 左右）下载。

#### 2. 离线安装包（可选）

下载离线包 `uv-x86_64-pc-windows-msvc.zip`，解压到**不含有空格**的目录，建议：`C:\tools\uv`

解压后应包含2个文件：

```Plain Text

C:\tools\uv\uv.exe
C:\tools\uv\uvx.exe
```

#### 添加到系统PATH（1分钟）

**方法A**：解压后将`uv.exe`复制到任意目录（如`D:\tools\uv`），通过以下步骤手动添加路径到系统环境变量：

1. 右键“此电脑”→“属性”→“高级系统设置”→“环境变量”

2. 在“系统变量”中找到`Path`，点击“编辑”→“新建”，输入文件所在路径（如`D:\tools\uv`）

3. 重启命令行工具生效

## 安装Specify CLI

### 首次安装

1. 管理员模式打开PowerShell并执行：

    ```Bash
    
    uv tool install specify-cli --from git+https://github.com/rj-wangbin6/spec-kit.git
    ```

2. 安装完成后执行`specify`，出现如下提示表明安装成功：

```Plain Text

Resolved 21 packages in 11.49s
Updated https://github.com/rj-wangbin6/spec-kit.git (e6d6f3cdee99752baee578896797400a72430ec0)
Built specify-cli @ git+https://github.com/rj-wangbin6/spec-kit.git@e6d6f3cdee99752baee578896797400a72430ec0
Prepared 21 packages in 23.72s
Installed 21 packages in 5.37s

anyio-4.11.0
certifi=2025.10.5
click-8.3.0
colorama-0.4.6
h11=0.16.0
httpcore-1.0.9
httpx-0.28.1
idna-3.11
markdown-it-py-4.0.0
mdurl=0.1.2
platformdirs-4.5.0
Pygments-2.19.2
readchar-4.2.1
rich=14.2.0
shellingham=1.5.4
sniffio-1.3.1
socksio-1.0.0
specify-cli=0.0.20 (from git+https://github.com/rj-wangbin6/spec-kit.git@e6d6f3cdee99752baee578896797400a72430ec0)
truststore-0.10.4
typer=0.20.0
typing-extensions-4.15.0

Installed 1 executable: specify

PS C:\Windows\system32> specify
GitHub Spec Kit - Spec-Driven Development Toolkit
Run 'specify --help' for usage information
```

## 初始化spec项目

1. 切换到项目根目录。示例：

    ```Bash
    
    cd C:\Users\admin\IdeaProjects\ai-system-admin
    ```

2. 执行初始化命令：

    ```Bash
    
    specify init . --ai copilot
    ```

3. 出现如下警告时输入`y`确认：

    ```Plain Text
    
    Warning: Current directory is not empty (13 items)
    Template files will be merged with existing content and may overwrite existing files
    Do you want to continue? [y/N]: y
    ```

4. 确认选择AI助手：`copilot`

5. 确认选择脚本类型：`ps`（PowerShell）

6. 提示`Project ready.`即初始化成功

7. `specify init` 完成后会自动执行 `.specify/scripts/powershell/post-init.ps1`

    - 若本机未安装 `git-ai`，会自动安装
    - 若本机已安装 `git-ai`，会自动尝试升级到最新可用版本
    - 完成后会自动刷新 `git-ai install-hooks`

8. 若需要验证本地开发分支版本，请参考独立文档：`Spec kit 本地仓库最小验证清单.md`

## 更新Specify CLI

使用以下命令可以实现把微软Specify CLI替换成锐捷版Specify CLI

### 升级specify

```Bash

uv tool install specify-cli --force --from git+https://github.com/rj-wangbin6/spec-kit.git
```

### 更新项目spec文件

注意：模板文件已更新/添加。已覆盖现有文件，但保留了项目中的其他文件。

1. 进入项目根目录

2. 执行更新命令：

    ```Bash
    
    specify init --here --force --ai copilot
    ```

3. 可重新选择更换其他模型和脚本类型（ps/sh）

4. 自动升级最新提示词文件、模板文件、脚本文件、宪法文件

5. 更新项目时也会自动执行 post-init

    - 若本机未安装 `git-ai`，会自动安装
    - 若本机已安装 `git-ai`，会自动尝试升级到最新可用版本
    - 完成后会自动刷新 `git-ai install-hooks`

6. 若需要基于本地仓库验证更新结果，请参考独立文档：`Spec kit 本地仓库最小验证清单.md`

### 批量升级所有项目

1. 打开任意项目的 `.specify/scripts/powershell` 目录

2. 检查是否存在 `batch-update.ps1` 文件（如果没有先按上面的非批量更新命令更新）

3. 在该目录新建 `project-dirs.conf` 文件，内容为**每行一个项目根目录**

4. 在 `.specify/scripts/powershell` 目录执行命令：

    - 使用默认配置文件：

        ```PowerShell
        
        .\batch-update.ps1
        ```

    - 使用自定义配置文件：

        ```PowerShell
        
        .\batch-update.ps1 -ConfigFile "D:\RjDir\UserData\Downloads\project-dirs.conf"
        ```

## 锐捷版修改内容

- 增加了适配锐捷开发规范的宪法

- 增加了`specify update`命令，支持批量升级更新项目spec相关文件

- 增加了根据项目目录名称自动在宪法中引入子宪法的能力

- 支持全程中文对话以及输出文件为中文，提升理解友好度

- 增加了codereview能力

## 安装常见问题

### 安装uvx：无法连接到远程服务器

当运行安装命令出现“无法连接到远程服务器”异常时：

1. 先执行命令开启TLS1.2：

    ```PowerShell
    
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    ```

2. 重新执行uv安装命令

3. 若仍失败，下载`uv-installer.ps1`文件，右键选择“使用PowerShell运行”，执行完成后继续后续步骤

### 安装specify-cli：卡住不动了，无法访问github

请私下联系团队里可以访问github的同事获取工具

### 安装specify-cli：Git operation failed... SSL certificate problem

错误信息示例：

```Plain Text

error: Git operation failed
Caused by: failed to fetch into: C:\Users\admin\AppData\Local\uv\cache\git-v0\db|2b1a97e5c98a6426
Caused by: process didn't exit successfully: C:\Program Files\Git\cmd\git.exe fetch --force --update-head-ok https://github.com/rj-wangbin6/spec-kit.git +HEAD:refs/remotes/origin/HEAD (exit code: 128)
stderr:
fatal: unable to access 'https://github.com/rj-wangbin6/spec-kit.git/': SSL certificate problem: unable to get local issuer certificate
```

解决方案：关闭Git SSL验证

```Bash

git config --global http.sslVerify false
```

## 效果图

- 由原版全程英文对话改为自动中文对话，无需额外指定使用中文

## 离线安装（不推荐）

1. 下载离线包 `spec.zip`

2. 解压缩后，右键单击`install-and-run-specify.ps1`选择“使用PowerShell运行”

3. 离线安装包含所有步骤但**不支持自动更新**，脚本提示词不是最新版，仅在无法在线安装时使用

4. 安装完成后直接进入**初始化spec项目**阶段

安装成功输出示例：

```Plain Text

执行策略更改
执行策略可帮助你防止执行不信任的脚本。更改执行策略可能会产生安全风险，如https://go.microsoft.com/fwlink/?LinkID=135170
中的about_Execution_Policies帮助主题所述。是否要更改执行策略?
[Y]是(Y)[A]全是(A)[N]否(N)[L]全否(L)[S]暂停(S)[?]帮助(默认值为"N"): A

Install and Run specify-cli Tool
===> Step 1: Check and install uv
[ok] uv is already installed: uv 0.9.9 (4fac4cb7e 2025-11-12)
Skip uv installation (use -ForceReinstallUv to reinstall)

===> Step 2: Use uv to install specify-cli
Installing specify-cli from local directory...
Local path: D:\project\ai-system\doc\spec\spec-kit
Command: uv tool install specify-cli --force --from D:\project\ai-system\doc\spec\spec-kit
Resolved 21 packages in 39ms
Uninstalled 1 package in 8ms
Installed 1 package in 151ms
specify-cli==0.0.20 (from file:///D:/project/ai-system/doc/spec/spec-kit)
Installed 1 executable: specify
[OK] specify-cli installation command executed

===> Step 3: Verify specify-cli installation
[ok] specify command is available!

Installation Summary
Available commands:
- Initialize a new Specify project: specify init
- Check required tools: specify check
- Update project with latest template: specify update

You can now use 'specify' command directly in your terminal!
Installation completed successfully!
Press any key to exit...
```

新开PowerShell执行`specify`看到提示则安装完成
> （注：文档部分内容可能由 AI 生成）