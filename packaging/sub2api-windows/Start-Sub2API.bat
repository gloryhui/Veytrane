@echo off
chcp 65001 >nul
setlocal
title Sub2API
cd /d "%~dp0"
if not exist "%~dp0Sub2API.exe" (
    echo Sub2API.exe was not found. Extract the complete ZIP and try again.
    pause
    exit /b 1
)
"%~dp0Sub2API.exe"
set "sub2api_exit_code=%errorlevel%"
if not "%sub2api_exit_code%"=="0" (
    echo.
    echo Connection stopped. Read the message above, then restart this BAT.
    pause
)
exit /b %sub2api_exit_code%
