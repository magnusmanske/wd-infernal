#!/bin/bash
toolforge webservice stop
toolforge webservice buildservice start --mount=all --health-check-path '/healthz'
