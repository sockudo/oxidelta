#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <file-or-directory>"
  exit 1
fi

input="$1"

transform_file() {
  local src="$1"
  local dst="${src}.migrated"

  perl -pe '
    if (/\bxdelta\b|\boxidelta\b/) {
      s/\bxdelta\s+-e\b/oxidelta encode/g;
      s/\bxdelta\s+-d\b/oxidelta decode/g;
      s/\bxdelta\s+printhdrs\b/oxidelta headers/g;
      s/\bxdelta\s+printhdr\b/oxidelta header/g;
      s/\bxdelta\s+printdelta\b/oxidelta delta/g;
      s/\bxdelta\s+merge\b/oxidelta merge/g;
      s/\bxdelta\s+config\b/oxidelta config/g;
      s/\bxdelta\b/oxidelta/g;

      s/(^|\s)-f(\s|$)/$1--force$2/g;
      s/(^|\s)-c(\s|$)/$1--stdout$2/g;
      s/(^|\s)-s(\s+)(\S+)/$1--source$2$3/g;
      s/(^|\s)-n(\s|$)/$1--no-checksum$2/g;
      s/(^|\s)-J(\s|$)/$1--check-only$2/g;
      s/(^|\s)-S(\s+)(\S+)/$1--secondary$2$3/g;
      s/(^|\s)-W(\s+)(\S+)/$1--window-size$2$3/g;
      s/(^|\s)-B(\s+)(\S+)/$1--source-window-size$2$3/g;
      s/(^|\s)-P(\s+)(\S+)/$1--duplicate-window-size$2$3/g;
      s/(^|\s)-I(\s+)(\S+)/$1--instruction-buffer-size$2$3/g;
      s/(^|\s)-m(\s+)(\S+)/$1--patch$2$3/g;
      s/(^|\s)-([0-9])(\s|$)/$1--level $2$3/g;
    }
  ' "$src" > "$dst"

  echo "migrated: $src -> $dst"
}

if [[ -f "$input" ]]; then
  transform_file "$input"
  exit 0
fi

if [[ -d "$input" ]]; then
  while IFS= read -r -d '' f; do
    transform_file "$f"
  done < <(find "$input" -type f \( -name "*.sh" -o -name "*.bash" -o -name "*.yml" -o -name "*.yaml" -o -name "*.md" -o -name "*.txt" \) -print0)
  exit 0
fi

echo "error: $input is not a file or directory"
exit 1
