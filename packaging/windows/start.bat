@echo off
rem Run the server by hand, from this folder whatever folder it was opened from.
rem For unattended running, use the scheduled task in README.txt instead.
cd /d "%~dp0"
server.exe
echo.
echo The server has stopped. Press any key to close this window.
pause >nul
