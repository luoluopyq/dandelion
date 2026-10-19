@echo off
REM One-click Windows installer build for Dandelion.
REM
REM   scripts\build-installer.cmd                  -> NSIS setup.exe
REM   scripts\build-installer.cmd -Bundler msi     -> MSI
REM   scripts\build-installer.cmd -Bundler all     -> both
REM
REM Exists because this machine's execution policy is Restricted, so a bare
REM   .\scripts\build-installer.ps1
REM is refused; this shim always passes -ExecutionPolicy Bypass.

setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-installer.ps1" %*
exit /b %ERRORLEVEL%
