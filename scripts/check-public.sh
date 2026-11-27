#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
failed=0

files=()
while IFS= read -r -d '' path; do
  [[ -f "$path" ]] && files+=("$path")
done < <(git ls-files -z --cached --others --exclude-standard)

# 1. Reject secret/local artifacts in tracked or unignored paths.
for path in "${files[@]}"; do
  case "$path" in
    .env.example) continue ;;
    .env|.env.*|*.pem|*.key|*.p12|*.cer|*.keystore|*.jks|*.mobileprovision|*.provisionprofile|*.apk|*.aab|*.sqlite|*.sqlite-*|*.db|.local/*|*.local.*)
      echo "ERROR: forbidden local/secret file is not ignored: $path" >&2
      failed=1
      ;;
  esac
done

# 2. Reject embedded private key headers and local home paths.
if [[ ${#files[@]} -gt 0 ]]; then
  if grep -InE -- "-----BEGIN (RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----" "${files[@]}" 2>/dev/null; then
    echo "ERROR: private key material found in repository" >&2
    failed=1
  fi
  scan_files=()
  for path in "${files[@]}"; do
    [[ "$path" == "scripts/check-public.sh" || "$path" == *Cargo.lock || "$path" == *pnpm-lock.yaml ]] && continue
    scan_files+=("$path")
  done
  if [[ ${#scan_files[@]} -gt 0 ]]; then
    if grep -InE -- "(/Users/|/home/|[A-Za-z]:\\\\Users\\\\)[A-Za-z0-9._-]+" "${scan_files[@]}" 2>/dev/null; then
      echo "ERROR: local machine home path found in repository" >&2
      failed=1
    fi
  fi
fi

# 3. If .env.local exists locally, enforce noreply commit email and scan non-placeholder values.
if [[ -f .env.local ]]; then
  git_email="$(git config user.email 2>/dev/null || true)"
  if [[ -n "$git_email" && "$git_email" != *@users.noreply.github.com ]]; then
    echo "ERROR: git user.email ($git_email) is not a GitHub noreply address" >&2
    failed=1
  fi
fi
if [[ -f .env.local && ${#files[@]} -gt 0 ]]; then
  while IFS='=' read -r k v; do
    [[ "$k" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    v="${v%$'\r'}"; v="${v%\"}"; v="${v#\"}"; v="${v%\'}"; v="${v#\'}"
    [[ ${#v} -lt 5 ]] && continue
    case "$v" in
      *example.com*|192.0.2.*|198.51.100.*|203.0.113.*|127.0.0.1*|localhost*|lab-http|admin|fesen|true|false) continue ;;
    esac
    if grep -InF -- "$v" "${files[@]}" 2>/dev/null; then
      echo "ERROR: local value from .env.local ($k) leaked into repository files" >&2
      failed=1
    fi
  done < .env.local
fi

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi
echo "OK: repository passed public safety checks"
