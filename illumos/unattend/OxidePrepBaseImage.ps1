$ErrorActionPreference = 'stop'

$setupDrive = "\\?\Volume{569CBD84-352D-44D9-B92D-BF25B852925B}\"

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
do {
    $ping = test-connection -Comp 1.1.1.1 -Count 1 -Quiet
    if ($stopwatch.elapsed -gt $timeout) {
        Write-Host "No internet connectivity"
        exit 1
    }
} while (-not $ping)
#endregion

#region Enable SSH
Write-Host "Enabling SSH"
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd

$content = [System.IO.File]::ReadAllText("C:\ProgramData\ssh\sshd_config").Replace("Match Group administrators", "#Match Group administrators").Replace("AuthorizedKeysFile __PROGRAMDATA__", "#AuthorizedKeysFile __PROGRAMDATA__")
[System.IO.File]::WriteAllText("C:\ProgramData\ssh\sshd_config", $content)
#endregion

#region Install Cloudbase-init (built from https://github.com/luqmana/cloudbase-init/tree/oxide w/ https://github.com/luqmana/cloudbase-init-installer/tree/oxide)
Write-Host "Installing cloudbase-init"
Invoke-WebRequest -Uri https://oxide-omicron-build.s3.amazonaws.com/CloudbaseInitSetup.msi -OutFile C:\Windows\Temp\CloudbaseInitSetup.msi | Out-Null
Start-Process msiexec.exe -ArgumentList "/i C:\Windows\Temp\CloudbaseInitSetup.msi /qn /norestart RUN_SERVICE_AS_LOCAL_SYSTEM=1" -Wait
del C:\Windows\Temp\CloudbaseInitSetup.msi

# Copy cloudbase-init configuration appropriate for Oxide rack
$confPath = "C:\Program Files\Cloudbase Solutions\Cloudbase-Init\conf\"
Copy-Item -LiteralPath "$setupDrive\cloudbase-init\cloudbase-init.conf" -Destination "$confPath\cloudbase-init.conf"
Copy-Item -LiteralPath "$setupDrive\cloudbase-init\cloudbase-init-unattend.conf" -Destination "$confPath\cloudbase-init-unattend.conf"
Remove-Item "$confPath\Unattend.xml" # We'll use our own instead (specialize-unattend.xml)

# Disable the service so it doesn't run on first boot and contend with the unattend first pass.
# We re-enable it during the specialize phase. See specialize-unattend.xml.
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
C:\Windows\System32\Sysprep\sysprep.exe /generalize /oobe /shutdown /unattend:"$setupDrive\specialize-unattend.xml"
#endregion
