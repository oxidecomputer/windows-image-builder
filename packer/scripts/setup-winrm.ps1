$ErrorActionPreference = 'Stop'

# Ensure PowerShell execution policy allows scripts.
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Force

# Enable and configure WinRM for Packer communication.
winrm quickconfig -quiet
Start-Service WinRM

# Allow unencrypted traffic and basic auth so Packer can connect.
# Use the WSMan provider instead of the winrm CLI to avoid @{} quoting issues.
Set-Item -Path WSMan:\localhost\Service\AllowUnencrypted -Value $true
Set-Item -Path WSMan:\localhost\Service\Auth\Basic -Value $true

# Open firewall for WinRM HTTP.
New-NetFirewallRule -DisplayName 'WinRM HTTP' -Direction Inbound -LocalPort 5985 -Protocol TCP -Action Allow

Restart-Service WinRM

Write-Host 'WinRM configured for Packer.'
