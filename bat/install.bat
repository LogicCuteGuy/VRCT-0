@echo off
setlocal
cd /d "%~dp0.."
call npm install
if errorlevel 1 exit /b %errorlevel%
call npm run native:prepare
exit /b %errorlevel%
