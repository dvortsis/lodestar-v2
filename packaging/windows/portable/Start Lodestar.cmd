@echo off
rem Starts Lodestar from this folder. Data is kept in the "data" folder beside it.
rem Press Ctrl+C and wait for shutdown to finish before closing this window.
cd /d "%~dp0"
set "LODESTAR_DATA_DIR=%~dp0data"
rem This window can be seen: a copy started by Restart now that fails at start
rem waits for Enter here instead of closing it unread.
set "LODESTAR_PORTABLE_WINDOW=1"
rem Open the web page once the server has had a moment to start.
start "" cmd /c "timeout /t 3 /nobreak >nul & start http://localhost:8090/"
rem An update interrupted between its two renames leaves no lodestar.exe here and
rem the previous version in data\bin: put it back. Never over an existing one.
if not exist "%~dp0lodestar.exe" if exist "%~dp0data\bin\previous.exe" move "%~dp0data\bin\previous.exe" "%~dp0lodestar.exe" >nul && echo Lodestar put the previous version back after an interrupted update.
echo Starting Lodestar. The first-run setup token is printed below; paste it into the page.
echo To stop, press Ctrl+C and wait for shutdown to finish before closing this window.
"%~dp0lodestar.exe" serve
rem 80: Restart now handed over to a new copy, which keeps running in this
rem window. Don't pause or say it stopped; Ctrl+C still stops it gracefully.
rem No labels: cmd can miss them in a file with Unix line endings.
if %errorlevel% equ 80 (
  echo.
  echo Lodestar restarted and keeps running in this window.
  echo To stop it, press Ctrl+C and wait for shutdown to finish before closing this window.
) else (
  echo.
  echo Lodestar stopped.
  pause
)
