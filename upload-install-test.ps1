[CmdletBinding()]
param(
    [Parameter(Mandatory = $false)][string]$GitAiExe,
    [Parameter(Mandatory = $false)][string]$GitPath,
    [Parameter(Mandatory = $false)][string]$RemoteUrl,
    [Parameter(Mandatory = $false)][string]$ApiKey,
    [Parameter(Mandatory = $false)][string]$UserId,
    [Parameter(Mandatory = $false)][string]$Source = 'installTest',
    [Parameter(Mandatory = $false)][string]$ProjectName = 'git-ai-install',
    [Parameter(Mandatory = $false)][string]$ReviewDocumentId,
    [Parameter(Mandatory = $false)][int]$TimeoutSec = 20,
    [Parameter(Mandatory = $false)][switch]$DryRun,
    [Parameter(Mandatory = $false)][switch]$Quiet,
    [Parameter(Mandatory = $false)][switch]$BestEffort
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$DefaultUploadUrl = 'https://service-gw.ruijie.com.cn/api/ai-cr-manage-service/api/public/upload/ai-stats'

function Write-InstallTestInfo {
    param([Parameter(Mandatory = $true)][string]$Message)
    if (-not $Quiet) {
        Write-Host $Message
    }
}

function Write-InstallTestWarning {
    param([Parameter(Mandatory = $true)][string]$Message)
    if (-not $Quiet) {
        Write-Warning $Message
    }
}

function Get-FirstNonEmptyValue {
    param([Parameter(Mandatory = $false)][AllowNull()][object[]]$Values)

    if ($null -eq $Values) {
        return $null
    }

    foreach ($value in $Values) {
        if ($null -eq $value) {
            continue
        }
        $text = ([string]$value).Trim()
        if ($text.Length -gt 0) {
            return $text
        }
    }

    return $null
}

function Resolve-GitAiExe {
    param([Parameter(Mandatory = $false)][string]$PreferredPath)

    if (-not [string]::IsNullOrWhiteSpace($PreferredPath) -and (Test-Path -LiteralPath $PreferredPath)) {
        return (Resolve-Path -LiteralPath $PreferredPath).Path
    }

    $homeCandidate = Join-Path $HOME '.git-ai\bin\git-ai.exe'
    if (Test-Path -LiteralPath $homeCandidate) {
        return $homeCandidate
    }

    foreach ($name in @('git-ai.exe', 'git-ai')) {
        $command = Get-Command $name -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($command -and $command.Path -and (Test-Path -LiteralPath $command.Path)) {
            return $command.Path
        }
    }

    throw 'git-ai executable was not found.'
}

function Invoke-CommandText {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $output = @(& $FilePath @Arguments 2>$null)
    if ($LASTEXITCODE -ne 0) {
        return $null
    }

    $text = ($output | Select-Object -First 1)
    if ($null -eq $text) {
        return $null
    }

    $trimmed = ([string]$text).Trim()
    if ($trimmed.Length -eq 0) {
        return $null
    }

    return $trimmed
}

function Get-GitAiVersion {
    param([Parameter(Mandatory = $true)][string]$ResolvedGitAiExe)

    $version = Invoke-CommandText -FilePath $ResolvedGitAiExe -Arguments @('--version')
    if (-not $version) {
        $version = Invoke-CommandText -FilePath $ResolvedGitAiExe -Arguments @('version')
    }
    if (-not $version) {
        throw 'Failed to read git-ai version.'
    }

    return $version
}

function Get-GitVersion {
    param([Parameter(Mandatory = $false)][string]$PreferredGitPath)

    $candidate = $null
    if (-not [string]::IsNullOrWhiteSpace($PreferredGitPath) -and (Test-Path -LiteralPath $PreferredGitPath)) {
        $candidate = $PreferredGitPath
    } else {
        $command = Get-Command git.exe -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($command -and $command.Path) {
            $candidate = $command.Path
        }
    }

    if (-not $candidate) {
        return $null
    }

    $version = Invoke-CommandText -FilePath $candidate -Arguments @('--version')
    if (-not $version) {
        return $null
    }

    return $version.Replace('git version ', '').Trim()
}

function Resolve-UploadUrl {
    param([Parameter(Mandatory = $false)][string]$PreferredUrl)

    $directUrl = Get-FirstNonEmptyValue -Values @($PreferredUrl, $env:GIT_AI_REPORT_REMOTE_URL)
    if ($directUrl) {
        return $directUrl
    }

    $endpoint = Get-FirstNonEmptyValue -Values @($env:GIT_AI_REPORT_REMOTE_ENDPOINT)
    $path = Get-FirstNonEmptyValue -Values @($env:GIT_AI_REPORT_REMOTE_PATH)
    if ($endpoint -and $path) {
        return ('{0}/{1}' -f $endpoint.TrimEnd('/'), $path.TrimStart('/'))
    }

    return $DefaultUploadUrl
}

function Normalize-IdeName {
    param([Parameter(Mandatory = $false)][string]$Name)

    if ([string]::IsNullOrWhiteSpace($Name)) {
        return $null
    }

    switch ($Name.Trim().ToLowerInvariant()) {
        'vscode' { return 'VS Code' }
        'code' { return 'VS Code' }
        'visual studio code' { return 'VS Code' }
        'cursor' { return 'Cursor' }
        'windsurf' { return 'Windsurf' }
        'intellij' { return 'IntelliJ IDEA' }
        'idea' { return 'IntelliJ IDEA' }
        'intellij idea' { return 'IntelliJ IDEA' }
        default { return $Name.Trim() }
    }
}

function New-SyntheticCommitSha {
    param([Parameter(Mandatory = $true)][string]$Version)

    $seed = '{0}|{1}|{2}' -f $Version, ([guid]::NewGuid().ToString('N')), (Get-Date).ToUniversalTime().ToString('o')
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($seed)
    $sha1 = [System.Security.Cryptography.SHA1]::Create()
    try {
        $hashBytes = $sha1.ComputeHash($bytes)
        return ([System.BitConverter]::ToString($hashBytes)).Replace('-', '').ToLowerInvariant()
    } finally {
        if ($sha1) {
            $sha1.Dispose()
        }
    }
}

function New-InstallTestPayload {
    param(
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $false)][string]$GitVersion,
        [Parameter(Mandatory = $true)][string]$CommitSha
    )

    $nowText = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
    $resolvedReviewDocumentId = $null
    if (-not [string]::IsNullOrWhiteSpace($ReviewDocumentId)) {
        $resolvedReviewDocumentId = $ReviewDocumentId
    }
    $ideName = Normalize-IdeName (Get-FirstNonEmptyValue -Values @(
            $env:GIT_AI_REPORT_IDE_NAME,
            $env:GIT_AI_IDE_NAME,
            $env:GIT_AI_EDITOR_NAME,
            $env:GIT_AI_EDITOR,
            $env:TERM_PROGRAM
        ))
    $ideVersion = Get-FirstNonEmptyValue -Values @(
        $env:GIT_AI_REPORT_IDE_VERSION,
        $env:GIT_AI_IDE_VERSION,
        $env:GIT_AI_EDITOR_VERSION,
        $env:TERM_PROGRAM_VERSION
    )
    $pluginVersion = Get-FirstNonEmptyValue -Values @(
        $env:GIT_AI_REPORT_PLUGIN_VERSION,
        $env:GIT_AI_PLUGIN_VERSION,
        $env:GIT_AI_REPORT_EXTENSION_VERSION,
        $env:GIT_AI_EXTENSION_VERSION
    )

    $toolModelBreakdown = [object[]]@(
        [ordered]@{
            tool = 'git-ai-installer'
            model = $Version
            aiAdditions = 0
            aiAccepted = 0
            mixedAdditions = 0
            totalAiAdditions = 0
            totalAiDeletions = 0
            timeWaitingForAi = 0
        }
    )

    $stats = [ordered]@{
        humanAdditions = 0
        unknownAdditions = 0
        mixedAdditions = 0
        aiAdditions = 0
        aiAccepted = 0
        totalAiAdditions = 0
        totalAiDeletions = 0
        gitDiffAddedLines = 0
        gitDiffDeletedLines = 0
        timeWaitingForAi = 0
        files = [object[]]@()
        toolModelBreakdown = $toolModelBreakdown
    }

    $prompt = [ordered]@{
        promptHash = $CommitSha
        tool = 'git-ai-installer'
        model = $Version
        humanAuthor = $null
        promptText = ('git-ai install success test. gitAiVersion={0}' -f $Version)
        messages = [object[]]@()
        messagesUrl = $null
        totalAdditions = 0
        totalDeletions = 0
        acceptedLines = 0
        overridenLines = 0
        customAttributes = [ordered]@{
            gitAiVersion = $Version
            installTest = 'true'
            installerScript = 'upload-install-test.ps1'
            source = $Source
        }
    }

    $commitItem = [ordered]@{
        commitSha = $CommitSha
        commitMessage = ('git-ai install success test ({0})' -f $Version)
        author = 'git-ai installer'
        timestamp = $nowText
        hasAuthorshipNote = $false
        stats = $stats
        prompts = [object[]]@($prompt)
    }

    return [ordered]@{
        repoUrl = 'git-ai-install-test'
        projectName = $ProjectName
        branch = 'install-success'
        source = $Source
        reviewDocumentId = $resolvedReviewDocumentId
        authorshipSchemaVersion = 'authorship/3.0.0'
        clientContext = [ordered]@{
            gitAiCliVersion = $Version
            gitAiPluginVersion = $pluginVersion
            ideName = $ideName
            ideVersion = $ideVersion
            gitVersion = $GitVersion
        }
        commits = [object[]]@($commitItem)
    }
}

function Send-InstallTestPayload {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][object]$Payload,
        [Parameter(Mandatory = $true)][string]$Version
    )

    $resolvedApiKey = Get-FirstNonEmptyValue -Values @($ApiKey, $env:GIT_AI_REPORT_REMOTE_API_KEY)
    $resolvedUserId = Get-FirstNonEmptyValue -Values @($UserId, $env:GIT_AI_REPORT_REMOTE_USER_ID)

    $headers = @{}
    if ($resolvedApiKey) {
        $headers['Authorization'] = ('Bearer {0}' -f $resolvedApiKey)
    }
    if ($resolvedUserId) {
        $headers['X-USER-ID'] = [string]$resolvedUserId
    }

    $json = $Payload | ConvertTo-Json -Depth 20 -Compress
    if ($DryRun) {
        if (-not $Quiet) {
            Write-Output $json
        }
        return [pscustomobject]@{
            Succeeded = $true
            DryRun = $true
            Url = $Url
            CommitSha = [string]$Payload['commits'][0]['commitSha']
        }
    }

    [void](Invoke-RestMethod -Uri $Url -Method POST -Body $json -Headers $headers -ContentType 'application/json' -UserAgent ('git-ai-install-test/{0}' -f $Version) -TimeoutSec $TimeoutSec)

    return [pscustomobject]@{
        Succeeded = $true
        DryRun = $false
        Url = $Url
        CommitSha = [string]$Payload['commits'][0]['commitSha']
    }
}

try {
    if ($env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD -eq '1' -or -not [string]::IsNullOrWhiteSpace($env:GIT_AI_TEST_DB_PATH)) {
        Write-InstallTestInfo 'Skipping git-ai install test upload.'
        return
    }

    $resolvedGitAiExe = Resolve-GitAiExe -PreferredPath $GitAiExe
    $version = Get-GitAiVersion -ResolvedGitAiExe $resolvedGitAiExe
    $gitVersion = Get-GitVersion -PreferredGitPath $GitPath
    $commitSha = New-SyntheticCommitSha -Version $version
    $url = Resolve-UploadUrl -PreferredUrl $RemoteUrl
    $payload = New-InstallTestPayload -Version $version -GitVersion $gitVersion -CommitSha $commitSha
    $result = Send-InstallTestPayload -Url $url -Payload $payload -Version $version

    if ($DryRun) {
        Write-InstallTestInfo ('Generated git-ai install test data ({0}) for dry-run.' -f $version)
    } else {
        Write-InstallTestInfo ('Sent git-ai install test data ({0}) to the remote dashboard.' -f $version)
    }
    if (-not $Quiet) {
        $result
    }
} catch {
    $message = 'Failed to send git-ai install test data to the remote dashboard: {0}' -f $_.Exception.Message
    if ($BestEffort) {
        Write-InstallTestWarning $message
        if (-not $Quiet) {
            [pscustomobject]@{
                Succeeded = $false
                Error = $_.Exception.Message
            }
        }
        return
    }

    throw $message
}