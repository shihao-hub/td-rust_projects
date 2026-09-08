@echo off
rem Run whoholds elevated WITHOUT a UAC prompt.
rem Writes the request to %TEMP%\whoholds_req.cmd, triggers the registered
rem highest-privilege scheduled task, waits for completion, prints output+exit code.
setlocal enableextensions
set "EXE=%~dp0..\target\release\whoholds.exe"
set "REQ=%TEMP%\whoholds_req.cmd"
set "OUT=%TEMP%\whoholds_out.txt"
set "DONE=%TEMP%\whoholds_done.tmp"

if not exist "%EXE%" (
    echo whoholds.exe not found: %EXE%
    echo run cargo build --release first
    exit /b 2
)
if "%~1"=="" (
    echo usage: whoholds-admin ^<file-path^> [options]
    echo        supports --all / --pid ^<PID^> / --json / --bench / --timeout N
    exit /b 2
)

if exist "%DONE%" del "%DONE%"
> "%REQ%" echo @"%EXE%" %* ^> "%OUT%" 2^>^&1
>> "%REQ%" echo @(echo %%errorlevel%%^)^>"%DONE%"

schtasks /Run /TN "whoholds_admin" >nul 2>&1
if errorlevel 1 (
    echo failed to start scheduled task - run scripts\install-task.cmd as admin first
    exit /b 2
)

set /a n=0
:wait
if exist "%DONE%" goto done
ping -n 2 127.0.0.1 >nul
set /a n+=1
if %n% lss 90 goto wait
echo timed out waiting for task ^(~180s^)
exit /b 2

:done
set /a code=0
set /p code=<"%DONE%"
chcp 65001 >nul
type "%OUT%"
del "%DONE%" >nul 2>&1
exit /b %code%
