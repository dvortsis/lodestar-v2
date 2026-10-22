@echo off
rem Starts Lodestar from this folder. Data is kept in the "data" folder beside it.
rem Close this window (or press Ctrl+C) to stop Lodestar.
cd /d "%~dp0"
set "LODESTAR_DATA_DIR=%~dp0data"
rem Open the web page once the server has had a moment to start.
start "" cmd /c "timeout /t 3 /nobreak >nul & start http://localhost:8090/"
echo Starting Lodestar. The first-run setup token is printed below; paste it into the page.
"%~dp0lodestar.exe" serve
echo.
echo Lodestar stopped.
pause
