#!/bin/bash
toolforge webservice buildservice restart --mount=all --health-check-path '/healthz' --mem 2G --cpu 2
