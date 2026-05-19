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

function Add-UniqueCandidatePath {
    param(
        [Parameter(Mandatory = $false)][System.Collections.ArrayList]$Paths,
        [Parameter(Mandatory = $false)][hashtable]$Seen,
        [Parameter(Mandatory = $false)][string]$Path
    )

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return
    }

    $trimmedPath = $Path.Trim()
    try {
        $normalizedPath = [System.IO.Path]::GetFullPath($trimmedPath)
    } catch {
        $normalizedPath = $trimmedPath
    }

    $pathKey = $normalizedPath.TrimEnd('\').ToLowerInvariant()
    if (-not $Seen.ContainsKey($pathKey)) {
        [void]$Paths.Add($normalizedPath)
        $Seen[$pathKey] = $true
    }
}

function Get-ExactPropertyValue {
    param(
        [Parameter(Mandatory = $false)][AllowNull()]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )

    if ($null -eq $Object) {
        return $null
    }

    if ($Object -is [System.Collections.IDictionary]) {
        return $Object[$Name]
    }

    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }

    return $property.Value
}

function Get-NestedPropertyValue {
    param(
        [Parameter(Mandatory = $false)][AllowNull()]$Object,
        [Parameter(Mandatory = $true)][string[]]$PropertyNames
    )

    $current = $Object
    foreach ($propertyName in $PropertyNames) {
        $current = Get-ExactPropertyValue -Object $current -Name $propertyName
        if ($null -eq $current) {
            return $null
        }
    }

    return $current
}

function Convert-ToNonEmptyString {
    param([Parameter(Mandatory = $false)][AllowNull()]$Value)

    if ($null -eq $Value) {
        return $null
    }

    if ($Value -is [string]) {
        $trimmed = $Value.Trim()
        if ($trimmed.Length -eq 0) {
            return $null
        }
        return $trimmed
    }

    if ($Value -is [ValueType]) {
        return ([string]$Value)
    }

    return $null
}

function Get-McpServerUserId {
    param([Parameter(Mandatory = $false)][AllowNull()]$Server)

    $requestInitHeaderUserId = Convert-ToNonEmptyString (Get-NestedPropertyValue -Object $Server -PropertyNames @('requestInit', 'headers', 'X-USER-ID'))
    if ($requestInitHeaderUserId) {
        return $requestInitHeaderUserId
    }

    return Convert-ToNonEmptyString (Get-NestedPropertyValue -Object $Server -PropertyNames @('headers', 'X-USER-ID'))
}

function Get-McpServerScore {
    param(
        [Parameter(Mandatory = $true)][string]$ServerName,
        [Parameter(Mandatory = $false)][AllowNull()]$Server
    )

    $score = 0
    if ($ServerName -in @('codereview-mcp', 'codereview-mcp-server')) {
        $score += 100
    }

    $url = Convert-ToNonEmptyString (Get-ExactPropertyValue -Object $Server -Name 'url')
    if ($url) {
        if ($url.Contains('mcppage.ruijie.com.cn:9810/mcp')) {
            $score += 50
        }
        if ($url.Contains('localhost:9810/mcp')) {
            $score += 25
        }
    }

    if (Get-McpServerUserId -Server $Server) {
        $score += 10
    }

    return $score
}

function Read-McpUserIdFromFile {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $null
    }

    try {
        $raw = [System.IO.File]::ReadAllText($Path)
    } catch {
        return $null
    }

    try {
        $root = $raw.TrimStart([char]0xFEFF) | ConvertFrom-Json
    } catch {
        return $null
    }

    $servers = Get-ExactPropertyValue -Object $root -Name 'servers'
    if ($null -eq $servers) {
        return $null
    }

    $candidates = @()
    foreach ($serverProperty in $servers.PSObject.Properties) {
        $candidates += [pscustomobject]@{
            Name = $serverProperty.Name
            Server = $serverProperty.Value
            Score = Get-McpServerScore -ServerName $serverProperty.Name -Server $serverProperty.Value
        }
    }

    foreach ($candidate in ($candidates | Sort-Object @{ Expression = 'Score'; Descending = $true }, @{ Expression = 'Name'; Descending = $false })) {
        $userId = Get-McpServerUserId -Server $candidate.Server
        if ($userId) {
            return $userId
        }
    }

    return $null
}

function Get-McpCandidatePaths {
    $paths = New-Object System.Collections.ArrayList
    $seen = @{}

    Add-UniqueCandidatePath -Paths $paths -Seen $seen -Path $env:GIT_AI_VSCODE_MCP_CONFIG_PATH
    Add-UniqueCandidatePath -Paths $paths -Seen $seen -Path $env:GIT_AI_IDEA_MCP_CONFIG_PATH

    try {
        $currentLocation = (Get-Location).Path
    } catch {
        $currentLocation = $null
    }
    if (-not [string]::IsNullOrWhiteSpace($currentLocation)) {
        Add-UniqueCandidatePath -Paths $paths -Seen $seen -Path (Join-Path $currentLocation '.vscode\mcp.json')
    }

    if (-not [string]::IsNullOrWhiteSpace($env:APPDATA)) {
        Add-UniqueCandidatePath -Paths $paths -Seen $seen -Path (Join-Path $env:APPDATA 'Code\User\mcp.json')
        Add-UniqueCandidatePath -Paths $paths -Seen $seen -Path (Join-Path $env:APPDATA 'Code - Insiders\User\mcp.json')
        Add-UniqueCandidatePath -Paths $paths -Seen $seen -Path (Join-Path $env:APPDATA 'github-copilot\intellij\mcp.json')
    }

    if (-not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        Add-UniqueCandidatePath -Paths $paths -Seen $seen -Path (Join-Path $env:LOCALAPPDATA 'github-copilot\intellij\mcp.json')
    }

    return [string[]]$paths
}

function Resolve-RemoteUserId {
    param([Parameter(Mandatory = $false)][string]$ExplicitUserId)

    $resolvedUserId = Get-FirstNonEmptyValue -Values @($ExplicitUserId, $env:GIT_AI_REPORT_REMOTE_USER_ID)
    if ($resolvedUserId) {
        return $resolvedUserId
    }

    foreach ($candidatePath in (Get-McpCandidatePaths)) {
        $resolvedUserId = Read-McpUserIdFromFile -Path $candidatePath
        if ($resolvedUserId) {
            return $resolvedUserId
        }
    }

    return $null
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
        [Parameter(Mandatory = $true)][string]$CommitSha,
        [Parameter(Mandatory = $false)][string]$ResolvedAuthor
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
        author = (Get-FirstNonEmptyValue -Values @($ResolvedAuthor, 'git-ai installer'))
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
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $false)][string]$ResolvedUserId
    )

    $resolvedApiKey = Get-FirstNonEmptyValue -Values @($ApiKey, $env:GIT_AI_REPORT_REMOTE_API_KEY)
    $resolvedUserId = Get-FirstNonEmptyValue -Values @($ResolvedUserId, $UserId, $env:GIT_AI_REPORT_REMOTE_USER_ID)

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
    $resolvedUserId = Resolve-RemoteUserId -ExplicitUserId $UserId
    $payload = New-InstallTestPayload -Version $version -GitVersion $gitVersion -CommitSha $commitSha -ResolvedAuthor $resolvedUserId
    $result = Send-InstallTestPayload -Url $url -Payload $payload -Version $version -ResolvedUserId $resolvedUserId

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