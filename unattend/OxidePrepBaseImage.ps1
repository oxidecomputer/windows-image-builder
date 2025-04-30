param (
    [Parameter(Mandatory=$True)]
    [string]$ConfigDir
)

$ErrorActionPreference = 'stop'

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

    $cmd = $ScriptBlock.ToString()
    Write-Error -Message "Command $cmd failed after $MaxAttempts attempts" -ErrorAction Stop
}

function DownloadLatestSshArchive {
    param (
        $ArchivePath
    )

    # GitHub requires clients to use at least TLS 1.2. Offer TLS 1.3 as well if
    # this version of Windows supports it.
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

    # The installation instructions on the Win32 OpenSSH wiki specify that
    # OpenSSH should live in "C:\Program Files\OpenSSH", so make sure it's
    # there and not in "OpenSSH-Win64".
    Expand-Archive -Path $ArchivePath -DestinationPath "C:\Program Files"
    Rename-Item -Path "C:\Program Files\OpenSSH-Win64" -NewName "C:\Program Files\OpenSSH"
    & "C:\Program Files\OpenSSH\install-sshd.ps1"
    New-NetFirewallRule -Name sshd -DisplayName 'OpenSSH Server (sshd)' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22
}

#region Enable serial console
Write-Host "Enabling Serial Console"
bcdedit /ems on
bcdedit /emssettings EMSPORT:1 EMSBAUDRATE:115200
#endregion

#region Enable Ping
Write-Host "Enabling Ping"
New-NetFirewallRule -DisplayName "Allow Inbound ICMPv4" -Direction Inbound -Protocol ICMPv4 -IcmpType 8 -RemoteAddress Any -Action Allow
#endregion

#region Enable RDP
Write-Host "Enabling RDP"
Set-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server\" -Name "fDenyTSConnections" -Value 0
Enable-NetFirewallRule -DisplayGroup "Remote Desktop"
#endregion

#region Wait for internet access
$timeout = New-TimeSpan -Seconds 30
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$connected = $false

do {
    $ping = Test-NetConnection -ComputerName "www.oxide.computer" -Port 443
    if ($ping.TcpTestSucceeded) {
        $connected = $true
        break
    }

    Start-Sleep -Seconds 1
} while ($stopwatch.Elapsed -lt $timeout)

if (-not $connected) {
    Write-Host "No internet connectivity"
    exit 1
} else {
    Write-Host "Internet connection established"
}
#endregion


#region Enable SSH
Write-Host "Enabling SSH"

# The easiest way to install OpenSSH is to install the relevant Windows
# capability, but this only exists in-box on Windows Server 2019 and later.
# Server 2016 recognizes the capability name, but enabling it doesn't actually
# install the sshd service. Try to detect both of these cases.
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 -ErrorAction SilentlyContinue
if ($?) {
    $sshCap = Get-Service -Name sshd -ErrorAction SilentlyContinue
}

# If either of the last two commands produced an error, fall back to trying to
# pull the latest release down from GitHub and trying to install it manually.
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
#endregion

#region Install Cloudbase-init (built from https://github.com/luqmana/cloudbase-init/tree/oxide w/ https://github.com/luqmana/cloudbase-init-installer/tree/oxide)
Write-Host "Installing cloudbase-init"
RetryWithBackoff -ScriptBlock { Invoke-WebRequest -Uri https://oxide-omicron-build.s3.amazonaws.com/CloudbaseInitSetup.msi -OutFile C:\Windows\Temp\CloudbaseInitSetup.msi | Out-Null }
Start-Process msiexec.exe -ArgumentList "/i C:\Windows\Temp\CloudbaseInitSetup.msi /qn /norestart RUN_SERVICE_AS_LOCAL_SYSTEM=1" -Wait
del C:\Windows\Temp\CloudbaseInitSetup.msi

# Copy cloudbase-init configuration appropriate for Oxide rack
$confPath = "C:\Program Files\Cloudbase Solutions\Cloudbase-Init\conf\"
Copy-Item "$ConfigDir\cloudbase-init.conf" -Destination "$confPath\cloudbase-init.conf"
Copy-Item "$ConfigDir\cloudbase-init-unattend.conf" -Destination "$confPath\cloudbase-init-unattend.conf"
Remove-Item "$confPath\Unattend.xml"

# Disable the service so it doesn't run on first boot and contend with the unattend first pass.
# We re-enable it during the specialize phase. See cloudbase-unattend.xml.
Set-Service -Name cloudbase-init -StartupType Disabled
#endregion

#region Cleanup and defrag/TRIM disk
Write-Host "Cleaning up disk"
Dism.exe /online /Cleanup-Image /StartComponentCleanup /ResetBase
Optimize-Volume -DriveLetter C
#endregion

#region Shrink OS partition
Write-Host "Shrinking OS partition"
$osPartition = Get-Partition -DriveLetter C
$resizeInfo = Get-PartitionSupportedSize -DriveLetter C
$minSz = $resizeInfo.SizeMin
$maxSz = $resizeInfo.SizeMax
$curSz = $osPartition.Size
$newSz = $minSz + 3GB
$diff = $curSz - $newSz
if ($newSz -lt $maxSz) { Resize-Partition -DriveLetter C -Size $newSz; Write-Host "New Partition Size: $newSz"; Write-Host "Free'd $diff" }
#endregion

#region Generalize image
Write-Host "Generalizing image"
C:\Windows\System32\Sysprep\sysprep.exe /generalize /oobe /shutdown /unattend:"$ConfigDir\specialize-unattend.xml"
#endregion
