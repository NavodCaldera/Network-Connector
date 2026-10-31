@echo off
rem Double-click to open Network Connector. The first run installs it (needs internet once).
cd /d "%~dp0"
if not exist ".venv\Scripts\pythonw.exe" (
    echo First run: installing, please wait...
    py -m venv .venv || python -m venv .venv || (echo Python 3.10+ is needed: https://www.python.org/downloads/ & pause & exit /b 1)
    ".venv\Scripts\python.exe" -m pip install -q -e ".[wifidirect]" || (echo Install failed. & pause & exit /b 1)
)
start "" ".venv\Scripts\pythonw.exe" -m network_connector.gui
