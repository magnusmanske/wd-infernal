#!/usr/bin/env bash
# Delopy and restart toolforge services
ssh login.toolforge.org 'become wd-infernal bash -lc "cd ~/wd-infernal/ && git pull && ./build.sh && ./restart.sh && echo \"OK\""'
