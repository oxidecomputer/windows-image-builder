mode COM1 BAUD=115200 PARITY=n DATA=8
powershell -ExecutionPolicy ByPass %1:\OxidePrepBaseImage.ps1 -ConfigDir %1: >\\.\COM1
pause
