#!/bin/bash
toolforge jobs delete rustbot
\rm ~/rustbot.*
toolforge jobs run --mem 2000Mi --cpu 2 --continuous --mount=all \
	--image tool-wd-infernal/tool-wd-infernal:latest \
	--command target/release/main \
	--filelog -o /data/project/wd-infernal/rustbot.out -e /data/project/wd-infernal/rustbot.err \
	rustbot
