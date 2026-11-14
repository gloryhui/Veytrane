@echo off
chcp 65001 >nul
cd /d "%~dp0"
"%~dp0Live-Viewer.exe"
if errorlevel 1 pause
