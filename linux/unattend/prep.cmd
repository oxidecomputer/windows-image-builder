mode COM1 BAUD=115200 PARITY=n DATA=8
dir %1:\
powershell -ExecutionPolicy ByPass %1:\OxidePrepBaseImage.ps1 >\\.\COM1
pause
