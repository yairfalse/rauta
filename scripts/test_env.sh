#!/bin/bash
export RAUTA_BACKEND_ADDR="127.0.0.1:8082"
export RAUTA_BIND_ADDR="127.0.0.1:9999"
"${RAUTA_CONTROL_PATH:-./target/release/control}"
