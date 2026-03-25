$ErrorActionPreference = 'Stop'

function RetryWithBackoff {
    param (
        [Parameter(Mandatory=$True)]
        [scriptblock]$ScriptBlock,

        [Parameter(Mandatory=$False)]
        [int]$MaxAttempts = 5,

        [Parameter(Mandatory=$False)]
        [int]$InitialBackoffDelayMs = 1000,

        [Parameter(Mandatory=$False)]
        [int]$MaxBackoffDelayMs = 30000
    )

    $cmd = $ScriptBlock.ToString()
    $cnt = 0
    $delay = $InitialBackoffDelayMs
    do {
        $cnt++
        try {
            Invoke-Command -Command $ScriptBlock
            return
        } catch {
            Write-Host "Command $cmd failed, will retry after $delay ms; error: " $_.Exception.InnerException.Message
            Start-Sleep -Milliseconds $delay
            $delay = [math]::Min($delay * 2, $MaxBackoffDelayMs)
        }
    } while ($cnt -lt $MaxAttempts)

    Write-Error -Message "Command $cmd failed after $MaxAttempts attempts" -ErrorAction Stop
}

function DownloadLatestSshArchive {
    param (
        $ArchivePath
    )

    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls13, [Net.SecurityProtocolType]::Tls12
    } catch {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    }

    $url = 'https://github.com/PowerShell/Win32-OpenSSH/releases/latest/'
    $request = [System.Net.WebRequest]::Create($url)
    $request.AllowAutoRedirect=$false
    $response = $request.GetResponse()
    $downloadPath = $([String]$response.GetResponseHeader("Location")).Replace('tag','download') + '/OpenSSH-Win64.zip'
    Write-Host "Downloading OpenSSH release from" $downloadPath
    Invoke-WebRequest -Uri $downloadPath -OutFile $ArchivePath | Out-Null
}

function InstallSshFromArchive {
    param (
        $ArchivePath
    )

    Expand-Archive -Path $ArchivePath -DestinationPath "C:\Program Files"
    Rename-Item -Path "C:\Program Files\OpenSSH-Win64" -NewName "C:\Program Files\OpenSSH"
    & "C:\Program Files\OpenSSH\install-sshd.ps1"
    New-NetFirewallRule -Name sshd -DisplayName 'OpenSSH Server (sshd)' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22
}

Write-Host "Enabling SSH"

# Try the Windows capability first (Server 2019+).
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 -ErrorAction SilentlyContinue
if ($?) {
    $sshCap = Get-Service -Name sshd -ErrorAction SilentlyContinue
}

if ($?) {
    Write-Host "SSH service installed via Add-WindowsCapability"
} else {
    Write-Host "SSH capability not present in image, will download from GitHub"
    $sshPath = "C:\Windows\Temp\OpenSSH-Win64.zip"
    RetryWithBackoff -ScriptBlock { DownloadLatestSshArchive -ArchivePath $sshPath }
    InstallSshFromArchive -ArchivePath $sshPath
}

Set-Service -Name sshd -StartupType Automatic
Start-Service sshd

$content = [System.IO.File]::ReadAllText("C:\ProgramData\ssh\sshd_config").Replace("Match Group administrators", "#Match Group administrators").Replace("AuthorizedKeysFile __PROGRAMDATA__", "#AuthorizedKeysFile __PROGRAMDATA__")
[System.IO.File]::WriteAllText("C:\ProgramData\ssh\sshd_config", $content)
