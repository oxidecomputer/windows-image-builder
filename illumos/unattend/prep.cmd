mode COM1 BAUD=115200 PARITY=n DATA=8
Powershell -ExecutionPolicy ByPass \\?\Volume`{569CBD84-352D-44D9-B92D-BF25B852925B`}\OxidePrepBaseImage.ps1 >\\.\COM1
pause
