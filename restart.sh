#!/bin/bash
toolforge webservice buildservice restart --mount=all --health-check-path '/healthz' --mem 4G --cpu 2
