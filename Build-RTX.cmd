@echo off
cd /d "%~dp0"
powershell.exe -NoProfile -File "%~dp0scripts\build_rtx.ps1" %*
if errorlevel 1 (
  echo.
  echo Build failed. Read the message above for the missing prerequisite or error.
  pause
  exit /b 1
)
pause
