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

Write-Host "Installing cloudbase-init"
RetryWithBackoff -ScriptBlock { Invoke-WebRequest -Uri https://oxide-omicron-build.s3.amazonaws.com/CloudbaseInitSetup.msi -OutFile C:\Windows\Temp\CloudbaseInitSetup.msi | Out-Null }
Start-Process msiexec.exe -ArgumentList "/i C:\Windows\Temp\CloudbaseInitSetup.msi /qn /norestart RUN_SERVICE_AS_LOCAL_SYSTEM=1" -Wait
Remove-Item C:\Windows\Temp\CloudbaseInitSetup.msi

# Copy cloudbase-init configuration from the floppy drive (A:\).
$confPath = "C:\Program Files\Cloudbase Solutions\Cloudbase-Init\conf\"
Copy-Item "A:\cloudbase-init.conf" -Destination "$confPath\cloudbase-init.conf"
Copy-Item "A:\cloudbase-init-unattend.conf" -Destination "$confPath\cloudbase-init-unattend.conf"
Remove-Item "$confPath\Unattend.xml"

# Disable the service so it doesn't run on first boot and contend with the
# unattend first pass. Re-enabled during the specialize phase.
Set-Service -Name cloudbase-init -StartupType Disabled
