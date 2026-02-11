#!/usr/bin/env bash
set -euo pipefail

# Minimal compatibility wrapper for common xdelta CLI invocations.
# It translates frequent legacy flag patterns into oxidelta syntax.

mode=""
args=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    -e)
      mode="encode"
      shift
      ;;
    -d)
      mode="decode"
      shift
      ;;
    -f)
      args+=("--force")
      shift
      ;;
    -c)
      args+=("--stdout")
      shift
      ;;
    -s)
      args+=("--source" "$2")
      shift 2
      ;;
    -n)
      args+=("--no-checksum")
      shift
      ;;
    -J)
      args+=("--check-only")
      shift
      ;;
    -S)
      args+=("--secondary" "$2")
      shift 2
      ;;
    -W)
      args+=("--window-size" "$2")
      shift 2
      ;;
    -B)
      args+=("--source-window-size" "$2")
      shift 2
      ;;
    -P)
      args+=("--duplicate-window-size" "$2")
      shift 2
      ;;
    -I)
      args+=("--instruction-buffer-size" "$2")
      shift 2
      ;;
    -m)
      args+=("--patch" "$2")
      shift 2
      ;;
    -[0-9])
      args+=("--level" "${1#-}")
      shift
      ;;
    config|merge|recode|printhdr|printhdrs|printdelta)
      case "$1" in
        printhdr) mode="header" ;;
        printhdrs) mode="headers" ;;
        printdelta) mode="delta" ;;
        *) mode="$1" ;;
      esac
      shift
      ;;
    *)
      args+=("$1")
      shift
      ;;
  esac
done

if [[ -z "$mode" ]]; then
  mode="encode"
fi

exec oxidelta "$mode" "${args[@]}"
