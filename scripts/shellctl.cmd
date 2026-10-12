@echo off
rem minicon-winmgr shell switcher — thin wrapper around shellctl.ps1
setlocal
set "PS1=%~dp0shellctl.ps1"

where pwsh >nul 2>nul
if %errorlevel%==0 (
  pwsh -NoProfile -ExecutionPolicy Bypass -File "%PS1%" %*
) else (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%PS1%" %*
)
exit /b %errorlevel%
