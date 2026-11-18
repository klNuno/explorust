#!/usr/bin/env bash
# Runs a command while holding the guest lock, so workers sharing g1 take turns (deploy, keys,
# captures). Waits up to 40 minutes for the lock. Usage: tools/g1-locked.sh <command> [args...]
exec flock -w "${G1_LOCK_WAIT:-2400}" /home/boite/projects/explorust/target/g1.lock "$@"
