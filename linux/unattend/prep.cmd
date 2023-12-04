mode COM1 BAUD=115200 PARITY=n DATA=8
Powershell -ExecutionPolicy ByPass %1:\OxidePrepBaseImage.ps1 >\\.\COM1
pause
