@echo off
setlocal
cd /d "%~dp0.."
call npm run tools:build-release
if errorlevel 1 exit /b %errorlevel%
call npm run tools:package-release
exit /b %errorlevel%
