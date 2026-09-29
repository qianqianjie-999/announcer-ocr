@echo off
cd /d %~dp0
python announce.py >> announce_run.log 2>&1
