@echo off
rem Starts Lodestar from this folder. Data is kept in the "data" folder beside it.
rem Press Ctrl+C and wait for shutdown to finish before closing this window.
cd /d "%~dp0"
set "LODESTAR_DATA_DIR=%~dp0data"
rem Open the web page once the server has had a moment to start.
start "" cmd /c "timeout /t 3 /nobreak >nul & start http://localhost:8090/"
echo Starting Lodestar. The first-run setup token is printed below; paste it into the page.
echo To stop, press Ctrl+C and wait for shutdown to finish before closing this window.
"%~dp0lodestar.exe" serve
echo.
echo Lodestar stopped.
pause
