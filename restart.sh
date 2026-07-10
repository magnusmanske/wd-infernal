#!/bin/bash
toolforge webservice stop
toolforge webservice buildservice start --mount=none --health-check-path '/healthz'
