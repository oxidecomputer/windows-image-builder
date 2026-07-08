$ErrorActionPreference = 'Stop'

# De-provision the build-time access paths before generalizing so none of them
# ship in the final image. This runs as an elevated scheduled task created by
# the Packer shutdown_command, after all provisioners have finished.

Write-Host 'Resetting WinRM configuration to defaults'
Set-Item -Path WSMan:\localhost\Service\AllowUnencrypted -Value $false
Set-Item -Path WSMan:\localhost\Service\Auth\Basic -Value $false
Remove-NetFirewallRule -DisplayName 'WinRM HTTP' -ErrorAction SilentlyContinue

Write-Host 'Removing autologon credentials'
$winlogon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
Set-ItemProperty -Path $winlogon -Name AutoAdminLogon -Value '0'
foreach ($name in 'DefaultPassword', 'DefaultUserName', 'DefaultDomainName', 'AutoLogonCount') {
    Remove-ItemProperty -Path $winlogon -Name $name -ErrorAction SilentlyContinue
}

# Scramble the well-known build password. The Administrator account itself is
# disabled during the specialize pass on first boot (see
# specialize-unattend.xml); this ensures the build credential is gone even if
# the account is ever re-enabled.
Write-Host 'Scrambling Administrator password'
$password = [guid]::NewGuid().ToString() + [guid]::NewGuid().ToString()
net user Administrator "$password" | Out-Null

# Remove build residue: the scheduled task that launched this script (the
# definition would otherwise ship in the image) and temp files left by the
# Packer provisioners. This script runs from A:\, so cleaning C:\ temp
# directories is safe.
Write-Host 'Removing build residue'
schtasks /delete /tn packer-sysprep /f | Out-Null
Remove-Item "$env:SystemRoot\Temp\*" -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item "$env:TEMP\*" -Recurse -Force -ErrorAction SilentlyContinue

Write-Host 'Generalizing image with sysprep'
& "$env:SystemRoot\System32\Sysprep\sysprep.exe" /generalize /oobe /shutdown /unattend:A:\specialize-unattend.xml
