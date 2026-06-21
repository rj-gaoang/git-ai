$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:isElevated = $false

function Start-DaemonIfRequested {
    if ($env:GIT_AI_RESTART_DAEMON_AFTER_INSTALL -ne '1') {
        return
    }
    if ($script:isElevated) {
        Write-Warning 'Skipping background service restart from elevated installer; it will start from the next normal git-ai command.'
        return
    }

    $daemonExe = Join-Path $HOME '.git-ai\launcher\git-ai.exe'
    if (-not (Test-Path $daemonExe)) {
        $daemonExe = Join-Path $HOME '.git-ai\bin\git-ai.exe'
    }
    if (-not (Test-Path $daemonExe)) {
        Write-Warning 'Warning: Failed to locate git-ai.exe for daemon restart after install.'
        return
    }

    try {
        & $daemonExe bg start *> $null
    } catch {
        Write-Warning 'Warning: Failed to restart git-ai background service automatically.'
    }
}

function Write-ErrorAndExit {
    param(
        [Parameter(Mandatory = $true)][string]$Message
    )
    Write-Host "Error: $Message" -ForegroundColor Red
    Start-DaemonIfRequested
    exit 1
}

function Write-Success {
    param(
        [Parameter(Mandatory = $true)][string]$Message
    )
    Write-Host $Message -ForegroundColor Green
}

function Write-Warning {
    param(
        [Parameter(Mandatory = $true)][string]$Message
    )
    Write-Host $Message -ForegroundColor Yellow
}

function Test-PassiveAutoUpdateMode {
    return $env:GIT_AI_DEFER_IF_BUSY -eq '1'
}

function Normalize-PathString {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )

    try {
        return ([IO.Path]::GetFullPath($Path.Trim())).TrimEnd('\').ToLowerInvariant()
    } catch {
        return ($Path.Trim()).TrimEnd('\').ToLowerInvariant()
    }
}

function Test-FileAvailable {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )

    try {
        $stream = [System.IO.File]::Open($Path, 'Open', 'Write', 'None')
        $stream.Close()
        return $true
    } catch {
        return $false
    }
}

function Register-DeleteOnReboot {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )

    try {
        if (-not ('GitAiMoveFileEx' -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class GitAiMoveFileEx {
    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    public static extern bool MoveFileEx(string existingFileName, string newFileName, int flags);
}
"@ -ErrorAction Stop
        }

        $moveFileDelayUntilReboot = 0x4
        return [GitAiMoveFileEx]::MoveFileEx($Path, $null, $moveFileDelayUntilReboot)
    } catch {
        return $false
    }
}

function Remove-OrScheduleDelete {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        return $true
    }

    try {
        Remove-Item -Force -LiteralPath $Path -ErrorAction Stop
        return $true
    } catch {
        if (Register-DeleteOnReboot -Path $Path) {
            Write-Warning "Scheduled stale git-ai binary for deletion on next reboot: $Path"
            return $true
        }

        Write-Warning "Warning: Failed to delete stale git-ai binary: $Path"
        return $false
    }
}

function Get-RetiredBinaryPath {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$InstallDir
    )

    $fileName = Split-Path -Leaf $Path
    $timestamp = Get-Date -Format 'yyyyMMddHHmmss'
    $basePath = Join-Path $InstallDir "$fileName.retired-$timestamp-$PID"
    $candidatePath = $basePath
    $suffix = 1

    while (Test-Path -LiteralPath $candidatePath) {
        $candidatePath = "$basePath-$suffix"
        $suffix += 1
    }

    return $candidatePath
}

function Stop-GitAiBackgroundService {
    param(
        [Parameter(Mandatory = $true)][string]$GitAiExe,
        [Parameter(Mandatory = $false)][switch]$Hard
    )

    if (-not (Test-Path -LiteralPath $GitAiExe)) {
        return $false
    }

    $commandArgs = @('bg', 'shutdown')
    if ($Hard) {
        $commandArgs += '--hard'
    }

    try {
        & $GitAiExe @commandArgs *> $null
        return $LASTEXITCODE -eq 0
    } catch {
        return $false
    }
}

function Get-GitAiManagedProcesses {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDir
    )

    $targetPaths = @(
        (Normalize-PathString (Join-Path $InstallDir 'git-ai.exe')),
        (Normalize-PathString (Join-Path $InstallDir 'git.exe'))
    )

    $processes = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
            if ($_.ProcessId -eq $PID) {
                return $false
            }

            if ($_.ExecutablePath -and ($targetPaths -contains (Normalize-PathString $_.ExecutablePath))) {
                return $true
            }

            if ($_.CommandLine) {
                $commandLine = $_.CommandLine.ToLowerInvariant()
                foreach ($targetPath in $targetPaths) {
                    if ($commandLine.Contains($targetPath)) {
                        return $true
                    }
                }
            }

            return $_.Name -ieq 'git-ai.exe'
        })

    return $processes
}

function Stop-ProcessTree {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId
    )

    try {
        $taskkillOutput = & taskkill.exe /F /T /PID $ProcessId 2>&1
        if ($LASTEXITCODE -eq 0) {
            return $true
        }
    } catch { }

    try {
        Stop-Process -Id $ProcessId -Force -ErrorAction Stop
        return $true
    } catch {
        return $false
    }
}

function Stop-GitAiManagedProcesses {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDir
    )

    $processes = @(Get-GitAiManagedProcesses -InstallDir $InstallDir)
    if ($processes.Count -eq 0) {
        return $false
    }

    $processIds = @($processes | Sort-Object ProcessId -Unique | Select-Object -ExpandProperty ProcessId)
    Write-Warning ("Stopping lingering git-ai processes: {0}" -f ($processIds -join ', '))

    foreach ($processId in $processIds) {
        [void](Stop-ProcessTree -ProcessId $processId)
    }

    return $true
}

function Install-BinaryWithRenameFallback {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$InstallDir,
        [Parameter(Mandatory = $true)][string]$Description
    )

    if (-not (Test-Path -LiteralPath $Destination)) {
        Move-Item -Force -LiteralPath $Source -Destination $Destination
        return
    }

    try {
        Move-Item -Force -LiteralPath $Source -Destination $Destination -ErrorAction Stop
        return
    } catch { }

    $retiredPath = Get-RetiredBinaryPath -Path $Destination -InstallDir $InstallDir
    $passiveAutoUpdate = Test-PassiveAutoUpdateMode
    try {
        Move-Item -Force -LiteralPath $Destination -Destination $retiredPath -ErrorAction Stop
        Write-Warning "Retired active $Description before install: $Destination -> $retiredPath"
    } catch {
        $retireError = $_.Exception.Message
        if ($passiveAutoUpdate) {
            Write-ErrorAndExit "Deferred auto-update because $Destination is still in use and could not be retired. git-ai will retry on a later update check."
        }

        Write-Warning "Could not retire active $Description before stopping processes: $retireError"
        $gitAiExe = Join-Path $InstallDir 'git-ai.exe'
        [void](Stop-GitAiBackgroundService -GitAiExe $gitAiExe -Hard)
        [void](Stop-GitAiManagedProcesses -InstallDir $InstallDir)

        try {
            Move-Item -Force -LiteralPath $Source -Destination $Destination -ErrorAction Stop
            return
        } catch { }

        try {
            Move-Item -Force -LiteralPath $Destination -Destination $retiredPath -ErrorAction Stop
            Write-Warning "Retired active $Description after stopping processes: $Destination -> $retiredPath"
        } catch {
            Write-ErrorAndExit "Failed to replace $Destination. Please close running git-ai processes and try again. $($_.Exception.Message)"
        }
    }

    try {
        Move-Item -Force -LiteralPath $Source -Destination $Destination -ErrorAction Stop
    } catch {
        try {
            if ((-not (Test-Path -LiteralPath $Destination)) -and (Test-Path -LiteralPath $retiredPath)) {
                Move-Item -Force -LiteralPath $retiredPath -Destination $Destination -ErrorAction SilentlyContinue
            }
        } catch { }
        Write-ErrorAndExit "Failed to install $Description to $Destination after retiring the old file. $($_.Exception.Message)"
    }

    [void](Remove-OrScheduleDelete -Path $retiredPath)
}

function Wait-ForFileAvailable {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$InstallDir,
        [Parameter(Mandatory = $false)][int]$MaxWaitSeconds = 300,
        [Parameter(Mandatory = $false)][int]$RetryIntervalSeconds = 5,
        [Parameter(Mandatory = $false)][int]$ForceKillAfterSeconds = 20
    )

    $elapsed = 0
    $gitAiExe = Join-Path $InstallDir 'git-ai.exe'
    $passiveAutoUpdate = Test-PassiveAutoUpdateMode
    $effectiveMaxWaitSeconds = $MaxWaitSeconds
    $effectiveRetryIntervalSeconds = $RetryIntervalSeconds

    if ($passiveAutoUpdate) {
        $effectiveMaxWaitSeconds = [Math]::Min($MaxWaitSeconds, 5)
        $effectiveRetryIntervalSeconds = 1
    }

    [void](Stop-GitAiBackgroundService -GitAiExe $gitAiExe)
    if (-not $passiveAutoUpdate) {
        [void](Stop-GitAiManagedProcesses -InstallDir $InstallDir)
    }

    while ($elapsed -lt $effectiveMaxWaitSeconds) {
        if (Test-FileAvailable -Path $Path) {
            return $true
        }

        if (-not $passiveAutoUpdate -and $elapsed -ge $ForceKillAfterSeconds) {
            [void](Stop-GitAiBackgroundService -GitAiExe $gitAiExe -Hard)
            [void](Stop-GitAiManagedProcesses -InstallDir $InstallDir)
        }

        if (-not (Test-FileAvailable -Path $Path)) {
            if ($elapsed -eq 0) {
                if ($passiveAutoUpdate) {
                    Write-Warning "git-ai is busy; deferring this auto-update instead of interrupting active work: $Path"
                } else {
                Write-Host "Waiting for file to be available: $Path" -ForegroundColor Yellow
                }
            }
            Start-Sleep -Seconds $effectiveRetryIntervalSeconds
            $elapsed += $effectiveRetryIntervalSeconds
        }
    }
    return $false
}

function Set-CurrentExePointer {
    param(
        [Parameter(Mandatory = $true)][string]$PointerPath,
        [Parameter(Mandatory = $true)][string]$TargetPath
    )

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $PointerPath) | Out-Null
    $tempPath = "{0}.tmp.{1}" -f $PointerPath, $PID
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($tempPath, $TargetPath, $utf8NoBom)
    Move-Item -Force -LiteralPath $tempPath -Destination $PointerPath
}

function Copy-InstalledBinary {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$InstallDir,
        [Parameter(Mandatory = $true)][string]$Description
    )

    $tempCopy = Join-Path $InstallDir ("{0}.tmp.{1}.exe" -f $Description, $PID)
    try {
        Copy-Item -Force -LiteralPath $Source -Destination $tempCopy -ErrorAction Stop
        Install-BinaryWithRenameFallback -Source $tempCopy -Destination $Destination -InstallDir $InstallDir -Description $Description
    } catch {
        Remove-Item -Force -ErrorAction SilentlyContinue $tempCopy
        throw
    }
}

function Invoke-GitAiInstallHooks {
    param([Parameter(Mandatory = $true)][string]$GitAiExe)

    $hadSkipInstallTestUpload = Test-Path Env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD
    $originalSkipInstallTestUpload = $env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD
    $hadDeferInstallHooksProbe = Test-Path Env:GIT_AI_DEFER_INSTALL_HOOKS_PROBE
    $originalDeferInstallHooksProbe = $env:GIT_AI_DEFER_INSTALL_HOOKS_PROBE
    $hadSkipDaemonRestart = Test-Path Env:GIT_AI_SKIP_DAEMON_RESTART
    $originalSkipDaemonRestart = $env:GIT_AI_SKIP_DAEMON_RESTART

    try {
        if ($env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD -eq '1' -and [string]::IsNullOrWhiteSpace($env:GIT_AI_TEST_DB_PATH)) {
            Remove-Item Env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD -ErrorAction SilentlyContinue
        }
        $env:GIT_AI_DEFER_INSTALL_HOOKS_PROBE = '1'
        if ($script:isElevated) {
            $env:GIT_AI_SKIP_DAEMON_RESTART = '1'
        }

        & $GitAiExe install-hooks | Out-Host
        if ($LASTEXITCODE -ne 0) {
            throw "git-ai install-hooks exited with code $LASTEXITCODE"
        }
    } finally {
        if ($hadSkipInstallTestUpload) {
            $env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD = $originalSkipInstallTestUpload
        } else {
            Remove-Item Env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD -ErrorAction SilentlyContinue
        }
        if ($hadDeferInstallHooksProbe) {
            $env:GIT_AI_DEFER_INSTALL_HOOKS_PROBE = $originalDeferInstallHooksProbe
        } else {
            Remove-Item Env:GIT_AI_DEFER_INSTALL_HOOKS_PROBE -ErrorAction SilentlyContinue
        }
        if ($hadSkipDaemonRestart) {
            $env:GIT_AI_SKIP_DAEMON_RESTART = $originalSkipDaemonRestart
        } else {
            Remove-Item Env:GIT_AI_SKIP_DAEMON_RESTART -ErrorAction SilentlyContinue
        }
    }
}

function Invoke-GitAiPostInstallProbe {
    param(
        [Parameter(Mandatory = $true)][string]$GitAiExe,
        [Parameter(Mandatory = $false)][string]$Status = 'success',
        [Parameter(Mandatory = $false)][string]$Stage = '',
        [Parameter(Mandatory = $false)][string]$Reason = ''
    )

    $hadSkipInstallTestUpload = Test-Path Env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD
    $originalSkipInstallTestUpload = $env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD

    try {
        if ($env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD -eq '1' -and [string]::IsNullOrWhiteSpace($env:GIT_AI_TEST_DB_PATH)) {
            Remove-Item Env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD -ErrorAction SilentlyContinue
        }

        $probeArgs = @('post-install-probe', '--status', $Status)
        if (-not [string]::IsNullOrWhiteSpace($Stage)) {
            $probeArgs += @('--stage', $Stage)
        }
        if (-not [string]::IsNullOrWhiteSpace($Reason)) {
            $probeArgs += @('--reason', $Reason)
        }

        & $GitAiExe @probeArgs | Out-Host
        if ($LASTEXITCODE -ne 0) {
            throw "git-ai post-install-probe exited with code $LASTEXITCODE"
        }
    } finally {
        if ($hadSkipInstallTestUpload) {
            $env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD = $originalSkipInstallTestUpload
        } else {
            Remove-Item Env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD -ErrorAction SilentlyContinue
        }
    }
}

function Get-UploadActivityLockPath {
    $internalDir = Join-Path $HOME '.git-ai\internal'
    New-Item -ItemType Directory -Force -Path $internalDir | Out-Null
    return Join-Path $internalDir 'upload_activity.lock'
}

function Acquire-UploadActivityLock {
    param(
        [Parameter(Mandatory = $false)][int]$MaxWaitSeconds = 300,
        [Parameter(Mandatory = $false)][int]$RetryIntervalMilliseconds = 250
    )

    $lockPath = Get-UploadActivityLockPath
    $elapsedMilliseconds = 0
    $maxWaitMilliseconds = $MaxWaitSeconds * 1000

    while ($elapsedMilliseconds -lt $maxWaitMilliseconds) {
        try {
            return [System.IO.File]::Open(
                $lockPath,
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
        } catch {
            Start-Sleep -Milliseconds $RetryIntervalMilliseconds
            $elapsedMilliseconds += $RetryIntervalMilliseconds
        }
    }

    Write-ErrorAndExit 'Timeout waiting for in-flight uploads to finish before install'
}

function Verify-Checksum {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string]$BinaryName
    )

    # Skip verification if no checksums are embedded
    if ($EmbeddedChecksums -eq '__CHECKSUMS_PLACEHOLDER__') {
        return
    }

    # Extract expected checksum for this binary
    $expected = $null
    $entries = $EmbeddedChecksums -split '\|'
    foreach ($entry in $entries) {
        if ($entry -match "^([0-9a-fA-F]+)\s+$([regex]::Escape($BinaryName))$") {
            $expected = $Matches[1]
            break
        }
    }

    if (-not $expected) {
        Write-ErrorAndExit "No checksum found for $BinaryName"
    }

    # Calculate actual checksum
    $hashCommand = Get-Command Get-FileHash -ErrorAction SilentlyContinue
    if ($null -ne $hashCommand) {
        $actual = (Get-FileHash -Path $File -Algorithm SHA256).Hash.ToLower()
    } else {
        $stream = [System.IO.File]::OpenRead($File)
        try {
            $sha256 = [System.Security.Cryptography.SHA256]::Create()
            $hashBytes = $sha256.ComputeHash($stream)
            $actual = ([System.BitConverter]::ToString($hashBytes)).Replace('-', '').ToLower()
        } finally {
            $stream.Dispose()
            if ($sha256) {
                $sha256.Dispose()
            }
        }
    }

    if ($expected -ne $actual) {
        Remove-Item -Force -ErrorAction SilentlyContinue $File
        Write-ErrorAndExit "Checksum verification failed for $BinaryName`nExpected: $expected`nActual:   $actual"
    }

    Write-Success "Checksum verified for $BinaryName"
}

# GitHub repository details
# Replaced during release builds with the actual repository (e.g., "git-ai-project/git-ai")
# When set to __REPO_PLACEHOLDER__, defaults to "git-ai-project/git-ai"
# Can be overridden at runtime with GIT_AI_GITHUB_REPO (e.g., "rj-gaoang/git-ai")
$Repo = '__REPO_PLACEHOLDER__'
if ($Repo -eq '__REPO_PLACEHOLDER__') {
    $Repo = 'git-ai-project/git-ai'
}
if (-not [string]::IsNullOrWhiteSpace($env:GIT_AI_GITHUB_REPO)) {
    $Repo = $env:GIT_AI_GITHUB_REPO.Trim()
}

# Version placeholder - replaced during release builds with actual version (e.g., "v1.0.24")
# When set to __VERSION_PLACEHOLDER__, defaults to "latest"
$PinnedVersion = '__VERSION_PLACEHOLDER__'

# Embedded checksums - replaced during release builds with actual SHA256 checksums
# Format: "hash  filename|hash  filename|..." (pipe-separated)
# When set to __CHECKSUMS_PLACEHOLDER__, checksum verification is skipped
$EmbeddedChecksums = '__CHECKSUMS_PLACEHOLDER__'

# Ensure TLS 1.2 for GitHub downloads on older PowerShell versions
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
} catch { }

function Join-UrlPath {
    param(
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [Parameter(Mandatory = $true)][string]$RelativePath
    )

    return ('{0}/{1}' -f $BaseUrl.TrimEnd('/'), $RelativePath.TrimStart('/'))
}

function Get-InstallerBaseUrl {
    param(
        [Parameter(Mandatory = $false)][string]$InstallerUrl
    )

    if ([string]::IsNullOrWhiteSpace($InstallerUrl)) {
        return $null
    }

    try {
        $uri = [System.Uri]$InstallerUrl.Trim()
        if (-not $uri.IsAbsoluteUri) {
            return $null
        }

        $host = $uri.Host.ToLowerInvariant()
        if ($host -eq 'api.github.com' -or $host -match '(^|\.)(github\.com|githubusercontent\.com)$') {
            return $null
        }

        $builder = New-Object System.UriBuilder($uri)
        $path = $builder.Path
        if ([string]::IsNullOrWhiteSpace($path)) {
            return $null
        }

        $trimmedPath = $path.TrimEnd('/')
        $lastSlash = $trimmedPath.LastIndexOf('/')
        if ($lastSlash -lt 0) {
            return $null
        }

        $builder.Path = if ($lastSlash -eq 0) {
            '/'
        } else {
            $trimmedPath.Substring(0, $lastSlash)
        }
        $builder.Query = ''
        $builder.Fragment = ''
        return $builder.Uri.AbsoluteUri.TrimEnd('/')
    } catch {
        return $null
    }
}

function Get-BinaryMirrorConfig {
    $explicitBaseUrl = $null
    foreach ($candidate in @($env:GIT_AI_BINARY_BASE_URL, $env:RUIJIE_AI_GIT_AI_BASE_URL)) {
        if (-not [string]::IsNullOrWhiteSpace($candidate)) {
            $explicitBaseUrl = $candidate.Trim().TrimEnd('/')
            break
        }
    }

    if ($explicitBaseUrl) {
        return [PSCustomObject]@{
            BaseUrl = $explicitBaseUrl
            IsSelfHosted = $true
        }
    }

    $derivedBaseUrl = Get-InstallerBaseUrl -InstallerUrl $env:GIT_AI_INSTALLER_URL
    if (-not $derivedBaseUrl) {
        return $null
    }

    return [PSCustomObject]@{
        BaseUrl = $derivedBaseUrl
        IsSelfHosted = $true
    }
}

function Get-Architecture {
    try {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
        switch ($arch) {
            'X64' { return 'x64' }
            'Arm64' { return 'arm64' }
            default { return $null }
        }
    } catch {
        $pa = $env:PROCESSOR_ARCHITECTURE
        if ($pa -match 'ARM64') { return 'arm64' }
        elseif ($pa -match '64') { return 'x64' }
        else { return $null }
    }
}

# Ensure $PathToAdd is first on the User PATH. No Machine PATH and no admin
# required. git-ai's git proxy must precede system Git for automatic commit
# attribution and upload followups to run.
function Set-PathEnsureContains {
    param(
        [Parameter(Mandatory = $true)][string]$PathToAdd
    )

    $sep = ';'

    function NormalizePath([string]$p) {
        try { return ([IO.Path]::GetFullPath($p.Trim())).TrimEnd('\\').ToLowerInvariant() }
        catch { return ($p.Trim()).TrimEnd('\\').ToLowerInvariant() }
    }

    $normalizedAdd = NormalizePath $PathToAdd

    try {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @()
        if ($userPath) { $entries = ($userPath -split $sep) | Where-Object { $_ -and $_.Trim() -ne '' } }
        $filteredEntries = @($entries | Where-Object { (NormalizePath $_) -ne $normalizedAdd })
        $alreadyFirst = ($entries.Count -gt 0 -and (NormalizePath $entries[0]) -eq $normalizedAdd)
        if ($alreadyFirst) {
            $userStatus = 'AlreadyPresent'
        } else {
            $newEntries = @($PathToAdd) + $filteredEntries
            $newUserPath = ($newEntries -join $sep)
            [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
            $userStatus = 'Updated'
        }
    } catch {
        $userStatus = 'Error'
    }

    # Update current process PATH immediately for this session
    try {
        $procPath = $env:PATH
        $procEntries = @()
        if ($procPath) { $procEntries = ($procPath -split $sep) | Where-Object { $_ -and $_.Trim() -ne '' } }
        $procAlreadyFirst = ($procEntries.Count -gt 0 -and (NormalizePath $procEntries[0]) -eq $normalizedAdd)
        if (-not $procAlreadyFirst) {
            $procFilteredEntries = @($procEntries | Where-Object { (NormalizePath $_) -ne $normalizedAdd })
            $env:PATH = ((@($PathToAdd) + $procFilteredEntries) -join $sep)
        }
    } catch { }

    return [PSCustomObject]@{
        UserStatus = $userStatus
    }
}

# Detect architecture and OS
$arch = Get-Architecture
if (-not $arch) { Write-ErrorAndExit "Unsupported architecture: $([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture)" }
$os = 'windows'

# Determine binary name and download URLs
$binaryName = "git-ai-$os-$arch"
$binaryMirrorConfig = Get-BinaryMirrorConfig
$binaryBaseUrl = if ($binaryMirrorConfig) { $binaryMirrorConfig.BaseUrl } else { $null }
$binaryMirrorIsSelfHosted = $binaryMirrorConfig -and $binaryMirrorConfig.IsSelfHosted
$mirrorDownloadUrlExe = if ($binaryBaseUrl) { Join-UrlPath -BaseUrl $binaryBaseUrl -RelativePath "$binaryName.exe" } else { $null }
$mirrorDownloadUrlNoExt = if ($binaryBaseUrl) { Join-UrlPath -BaseUrl $binaryBaseUrl -RelativePath $binaryName } else { $null }

# Determine release tag
# Priority: 1. Local binary override, 2. Pinned version (for release builds), 3. Environment variable, 4. "latest"
if (-not [string]::IsNullOrWhiteSpace($env:GIT_AI_LOCAL_BINARY)) {
    $releaseTag = 'local'
} elseif ($PinnedVersion -ne '__VERSION_PLACEHOLDER__') {
    # Version-pinned install script from a release
    $releaseTag = $PinnedVersion
    $downloadUrlExe = "https://github.com/$Repo/releases/download/$releaseTag/$binaryName.exe"
    $downloadUrlNoExt = "https://github.com/$Repo/releases/download/$releaseTag/$binaryName"
} elseif (-not [string]::IsNullOrWhiteSpace($env:GIT_AI_RELEASE_TAG) -and $env:GIT_AI_RELEASE_TAG -ne 'latest') {
    # Environment variable override
    $releaseTag = $env:GIT_AI_RELEASE_TAG
    $downloadUrlExe = "https://github.com/$Repo/releases/download/$releaseTag/$binaryName.exe"
    $downloadUrlNoExt = "https://github.com/$Repo/releases/download/$releaseTag/$binaryName"
} else {
    # Default to latest
    $releaseTag = 'latest'
    $downloadUrlExe = "https://github.com/$Repo/releases/latest/download/$binaryName.exe"
    $downloadUrlNoExt = "https://github.com/$Repo/releases/latest/download/$binaryName"
}

# ============================================================
# Warn when installing as Administrator (not recommended).
# Running elevated creates files that normal-user processes
# cannot access, causing persistent daemon lock failures.
# ============================================================
$isElevated = $false
try {
    # Detect explicit UAC elevation ("Run as Administrator") via TokenElevationType.
    # Type 1 (Default) = no split token (UAC disabled or built-in Admin) -> no warn
    # Type 2 (Full)    = elevated half of a split token -> WARN (this is the danger case)
    # Type 3 (Limited) = non-elevated half of a split token -> no warn
    # We only warn on type 2: user explicitly elevated, so files will be admin-owned
    # but normal processes won't be, causing the daemon.lock mismatch from issue #1287.
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class GitAiElevation {
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool OpenProcessToken(IntPtr h, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool GetTokenInformation(IntPtr token, int cls, ref int info, int len, out int ret);
    [DllImport("kernel32.dll")]
    static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr h);
    public static bool IsElevated() {
        IntPtr tok;
        if (!OpenProcessToken(GetCurrentProcess(), 0x0008, out tok)) return false;
        try {
            int elevType = 0; int sz;
            // TokenElevationType = class 18; returns 1/2/3
            if (!GetTokenInformation(tok, 18, ref elevType, 4, out sz)) return false;
            return elevType == 2; // TokenElevationTypeFull
        } finally { CloseHandle(tok); }
    }
}
"@ -ErrorAction SilentlyContinue
    $isElevated = [GitAiElevation]::IsElevated()
} catch { }

if ($isElevated -and $env:GIT_AI_ALLOW_SUPERUSER -ne '1') {
    # Auto-allow in CI environments and daemon-triggered self-updates
    $isCi = $env:CI -or $env:GITHUB_ACTIONS -or $env:GITLAB_CI -or $env:JENKINS_URL `
        -or $env:BUILDKITE -or $env:CIRCLECI -or $env:CODEBUILD_BUILD_ID `
        -or $env:AGENT_OS -or $env:KUBERNETES_SERVICE_HOST `
        -or $env:GIT_AI_DAEMON_UPGRADE -or $env:container

    if (-not $isCi) {
        Write-Host ''
        Write-Host 'Warning: installing git-ai as Administrator is not recommended.' -ForegroundColor Yellow
        Write-Host ''
        Write-Host 'Running with elevated privileges creates files owned by Administrator that'
        Write-Host 'become inaccessible to your normal user account, causing persistent daemon'
        Write-Host 'lock failures. A future version may refuse to install in this configuration.'
        Write-Host ''
        Write-Host 'To suppress this warning, either:'
        Write-Host '  - Run this installer from a normal (non-elevated) PowerShell window (recommended), or'
        Write-Host '  - Set $env:GIT_AI_ALLOW_SUPERUSER = "1"' -ForegroundColor Yellow
        Write-Host ''
    }
    # Propagate to child git-ai invocations (install-hooks, exchange-nonce, login)
    $env:GIT_AI_ALLOW_SUPERUSER = '1'
}

# Install directories:
# - launcher is the authoritative entrypoint used by the update service and current-exe.
# - bin remains a compatibility copy for existing PATH and agent hook configurations.
$gitAiRoot = Join-Path $HOME '.git-ai'
$launcherDir = Join-Path $gitAiRoot 'launcher'
$installDir = Join-Path $gitAiRoot 'bin'
New-Item -ItemType Directory -Force -Path $launcherDir | Out-Null
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

if ($binaryBaseUrl) {
    Write-Host ("Downloading git-ai (base: {0}, release: {1})..." -f $binaryBaseUrl, $releaseTag)
} else {
    Write-Host ("Downloading git-ai (repo: {0}, release: {1})..." -f $Repo, $releaseTag)
}
$tmpFile = Join-Path $launcherDir "git-ai.tmp.$PID.exe"

function Try-Download {
    param(
        [Parameter(Mandatory = $true)][string]$Url
    )
    try {
        # Disable progress bar to avoid extreme slowdown caused by PowerShell's
        # progress-stream rendering (can make downloads 10-50x slower).
        $oldProgressPreference = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        try {
            Invoke-WebRequest -Uri $Url -OutFile $tmpFile -UseBasicParsing -ErrorAction Stop
        } finally {
            $ProgressPreference = $oldProgressPreference
        }
        return $true
    } catch {
        return $false
    }
}

function Get-GitHubReleaseApiUrl {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string]$ReleaseTag
    )

    if ($ReleaseTag -eq 'latest') {
        return "https://api.github.com/repos/$Repository/releases/latest"
    }

    return "https://api.github.com/repos/$Repository/releases/tags/$ReleaseTag"
}

function Try-DownloadFromGitHubApiAsset {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string]$ReleaseTag,
        [Parameter(Mandatory = $true)][string[]]$CandidateAssetNames
    )

    $apiUrl = Get-GitHubReleaseApiUrl -Repository $Repository -ReleaseTag $ReleaseTag
    $headers = @{
        'Accept' = 'application/vnd.github+json'
        'User-Agent' = 'git-ai-installer'
        'X-GitHub-Api-Version' = '2022-11-28'
    }

    try {
        $release = Invoke-RestMethod -Uri $apiUrl -Headers $headers -ErrorAction Stop
    } catch {
        return $null
    }

    foreach ($assetName in $CandidateAssetNames) {
        $asset = @($release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1)
        if (-not $asset) {
            continue
        }

        try {
            $oldProgressPreference = $ProgressPreference
            $ProgressPreference = 'SilentlyContinue'
            try {
                Invoke-WebRequest -Uri $asset.url -Headers @{
                    'Accept' = 'application/octet-stream'
                    'User-Agent' = 'git-ai-installer'
                    'X-GitHub-Api-Version' = '2022-11-28'
                } -OutFile $tmpFile -UseBasicParsing -ErrorAction Stop
            } finally {
                $ProgressPreference = $oldProgressPreference
            }
            return $assetName
        } catch {
            Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
        }
    }

    return $null
}

# Track which download URL succeeded for checksum verification
$downloadedBinaryName = $null
if (-not [string]::IsNullOrWhiteSpace($env:GIT_AI_LOCAL_BINARY)) {
    if (-not (Test-Path -LiteralPath $env:GIT_AI_LOCAL_BINARY)) {
        Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
        Write-ErrorAndExit "Local binary not found at $($env:GIT_AI_LOCAL_BINARY)"
    }
    Copy-Item -Force -Path $env:GIT_AI_LOCAL_BINARY -Destination $tmpFile
    $downloadedBinaryName = "$binaryName.exe"
} else {
    if ($mirrorDownloadUrlExe -and (Try-Download -Url $mirrorDownloadUrlExe)) {
        $downloadedBinaryName = "$binaryName.exe"
    } elseif ($mirrorDownloadUrlNoExt -and (Try-Download -Url $mirrorDownloadUrlNoExt)) {
        $downloadedBinaryName = $binaryName
    } elseif ($binaryMirrorIsSelfHosted) {
        Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
        Write-ErrorAndExit ("Failed to download binary from {0}" -f $binaryBaseUrl)
    } elseif (Try-Download -Url $downloadUrlExe) {
        $downloadedBinaryName = "$binaryName.exe"
    } elseif (Try-Download -Url $downloadUrlNoExt) {
        $downloadedBinaryName = $binaryName
    } else {
        $downloadedBinaryName = Try-DownloadFromGitHubApiAsset -Repository $Repo -ReleaseTag $releaseTag -CandidateAssetNames @(
            "$binaryName.exe",
            $binaryName
        )
    }
}

if (-not $downloadedBinaryName) {
    Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
    Write-ErrorAndExit 'Failed to download binary (HTTP error)'
}

try {
    if ((Get-Item $tmpFile).Length -le 0) {
        Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
        Write-ErrorAndExit 'Downloaded file is empty'
    }
} catch {
    Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
    Write-ErrorAndExit 'Download failed'
}

# Verify checksum if embedded (release builds only)
Verify-Checksum -File $tmpFile -BinaryName $downloadedBinaryName

$uploadActivityLock = Acquire-UploadActivityLock

$launcherExe = Join-Path $launcherDir 'git-ai.exe'
$finalExe = Join-Path $installDir 'git-ai.exe'
$gitShim = Join-Path $installDir 'git.exe'
$currentExePointer = Join-Path $gitAiRoot 'current-exe'

Install-BinaryWithRenameFallback -Source $tmpFile -Destination $launcherExe -InstallDir $launcherDir -Description 'launcher git-ai.exe'
try { Unblock-File -Path $launcherExe -ErrorAction SilentlyContinue } catch { }
Set-CurrentExePointer -PointerPath $currentExePointer -TargetPath $launcherExe

Copy-InstalledBinary -Source $launcherExe -Destination $finalExe -InstallDir $installDir -Description 'compatibility git-ai.exe'
try { Unblock-File -Path $finalExe -ErrorAction SilentlyContinue } catch { }

# Keep git.exe installed beside git-ai.exe. The git proxy is what sends wrapper
# pre/post state for commit processing and triggers post-commit upload followups.
Copy-InstalledBinary -Source $launcherExe -Destination $gitShim -InstallDir $installDir -Description 'git proxy git.exe'
try { Unblock-File -Path $gitShim -ErrorAction SilentlyContinue } catch { }

# Login user with install token if provided
$needLogin = $false
if ($env:INSTALL_NONCE -and $env:API_BASE) {
    try {
        & $launcherExe exchange-nonce | Out-Host
        if ($LASTEXITCODE -ne 0) {
            $needLogin = $true
        }
    } catch {
        $needLogin = $true
    }
}

# Install hooks
Write-Host 'Setting up IDE/agent hooks...'
$installHooksSucceeded = $false
try {
    Invoke-GitAiInstallHooks -GitAiExe $launcherExe
    $installHooksSucceeded = $true
    Write-Success 'Successfully set up IDE/agent hooks'
} catch {
    $installHooksError = $_.Exception.Message
    Write-Warning "Warning: Failed to set up IDE/agent hooks. Please try running 'git-ai install-hooks' manually."
    try {
        Invoke-GitAiPostInstallProbe -GitAiExe $launcherExe -Status 'failed' -Stage 'install-hooks' -Reason $installHooksError
    } catch {
        Write-Warning "Warning: Failed to send git-ai failed-install probe. Dashboard install telemetry may be delayed."
    }
}

# Best-effort restart only for daemon-initiated self-updates.
Start-DaemonIfRequested

$skipPathUpdate = $env:GIT_AI_SKIP_PATH_UPDATE -eq '1'
if ($skipPathUpdate) {
    Write-Warning 'Skipping PATH updates because GIT_AI_SKIP_PATH_UPDATE=1'
    $pathUpdate = [PSCustomObject]@{
        UserStatus = 'Skipped'
    }
} else {
    $pathUpdate = Set-PathEnsureContains -PathToAdd $installDir
}
if ($pathUpdate.UserStatus -eq 'Updated') {
    Write-Success 'Successfully added git-ai to the user PATH.'
} elseif ($pathUpdate.UserStatus -eq 'AlreadyPresent') {
    Write-Success 'git-ai already present in the user PATH.'
} elseif ($pathUpdate.UserStatus -eq 'Error') {
    Write-Host 'Failed to update the user PATH.' -ForegroundColor Red
}

Write-Success "Successfully installed git-ai into $launcherDir"
Write-Success "Synchronized git-ai and git proxy entrypoints into $installDir"
Write-Success "You can now run 'git-ai' and git-ai-managed 'git' from your terminal"

if ($installHooksSucceeded) {
    try {
        Invoke-GitAiPostInstallProbe -GitAiExe $launcherExe -Status 'success'
    } catch {
        Write-Warning "Warning: Failed to send git-ai install success probe. Dashboard install telemetry may be delayed."
    }
}

# Configure Git Bash shell profiles so git-ai takes precedence over /mingw64/bin/git
# Git Bash (MSYS2/MinGW) prepends its own directories to PATH, which shadows
# the Windows PATH entry we set above. Writing to ~/.bashrc ensures git-ai's
# bin directory is prepended after Git Bash's own PATH setup.
$gitBashConfigured = $false
$gitBashAlreadyConfigured = $false
try {
    $bashrcPath = Join-Path $HOME '.bashrc'
    $bashProfilePath = Join-Path $HOME '.bash_profile'
    $pathCmd = 'export PATH="$HOME/.git-ai/bin:$PATH"'
    $markerString = '.git-ai/bin'

    # Detect if Git Bash is installed
    $gitBashInstalled = $false
    $gitForWindowsPaths = @()
    if ($env:ProgramFiles) { $gitForWindowsPaths += Join-Path $env:ProgramFiles 'Git\bin\bash.exe' }
    if (${env:ProgramFiles(x86)}) { $gitForWindowsPaths += Join-Path ${env:ProgramFiles(x86)} 'Git\bin\bash.exe' }
    if ($env:LOCALAPPDATA) { $gitForWindowsPaths += Join-Path $env:LOCALAPPDATA 'Programs\Git\bin\bash.exe' }
    foreach ($p in $gitForWindowsPaths) {
        if ($p -and (Test-Path -LiteralPath $p)) {
            $gitBashInstalled = $true
            break
        }
    }

    if ($gitBashInstalled) {
        # Determine which config file to update (prefer .bashrc, fall back to .bash_profile)
        $targetBashConfig = $null
        if (Test-Path -LiteralPath $bashrcPath) {
            $targetBashConfig = $bashrcPath
        } elseif (Test-Path -LiteralPath $bashProfilePath) {
            $targetBashConfig = $bashProfilePath
        } else {
            # No existing config; create .bashrc
            $targetBashConfig = $bashrcPath
        }

        # Check if already configured
        $alreadyPresent = $false
        if (Test-Path -LiteralPath $targetBashConfig) {
            $content = Get-Content -LiteralPath $targetBashConfig -Raw -ErrorAction SilentlyContinue
            if ($content -and $content.Contains($markerString)) {
                $alreadyPresent = $true
            }
        }

        if ($alreadyPresent) {
            $gitBashAlreadyConfigured = $true
        } else {
            $timestamp = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'
            $appendContent = "`n# Added by git-ai installer on $timestamp`n$pathCmd`n"
            $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
            [System.IO.File]::AppendAllText($targetBashConfig, $appendContent, $utf8NoBom)
            $gitBashConfigured = $true
        }
    }
} catch {
    Write-Host "Warning: Failed to configure Git Bash: $($_.Exception.Message)" -ForegroundColor Yellow
}

if ($gitBashConfigured) {
    Write-Success "Successfully configured Git Bash ($targetBashConfig)"
} elseif ($gitBashAlreadyConfigured) {
    Write-Success "Git Bash already configured ($targetBashConfig)"
}

if ($uploadActivityLock) {
    $uploadActivityLock.Dispose()
    $uploadActivityLock = $null
}

Write-Host 'Close and reopen your terminal and IDE sessions to use git-ai.' -ForegroundColor Yellow

# If nonce exchange failed, run interactive login
if ($needLogin) {
    Write-Host ''
    Write-Host 'Launching login...'
    & $finalExe login
}
