@echo off
setlocal
cd /d "%~dp0.."
call npm run build-cuda
exit /b %errorlevel%
