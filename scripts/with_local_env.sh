#!/usr/bin/env bash
set -euo pipefail

krw_env_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_env_file="$krw_env_root/.env.local"

if [[ $# -eq 0 ]]; then
  printf 'usage: %s command [argument ...]\n' "$0" >&2
  exit 2
fi
if [[ ! -f "$krw_env_file" || -L "$krw_env_file" ]]; then
  printf 'missing regular local secret file: %s\n' "$krw_env_file" >&2
  exit 2
fi

if ! krw_env_mode=$(stat -f '%Lp' "$krw_env_file" 2>/dev/null); then
  krw_env_mode=$(stat -c '%a' "$krw_env_file")
fi
if (( (8#$krw_env_mode & 8#077) != 0 )); then
  printf 'local secret file must be mode 0600: %s\n' "$krw_env_file" >&2
  exit 2
fi

krw_env_key=''
krw_env_seen=false
krw_env_glm_key=''
krw_env_glm_seen=false
while IFS= read -r krw_env_line || [[ -n "$krw_env_line" ]]; do
  if [[ -z "$krw_env_line" || "$krw_env_line" == \#* ]]; then
    continue
  fi
  if [[ "$krw_env_line" =~ ^DEEPSEEK_API_KEY=([^[:space:]#]+)$ ]]; then
    if [[ "$krw_env_seen" == true ]]; then
      printf 'local secret file defines DEEPSEEK_API_KEY more than once\n' >&2
      exit 2
    fi
    krw_env_key=${BASH_REMATCH[1]}
    krw_env_seen=true
    continue
  fi
  if [[ "$krw_env_line" =~ ^GLM_API_KEY=([^[:space:]#]+)$ ]]; then
    if [[ "$krw_env_glm_seen" == true ]]; then
      printf 'local secret file defines GLM_API_KEY more than once\n' >&2
      exit 2
    fi
    krw_env_glm_key=${BASH_REMATCH[1]}
    krw_env_glm_seen=true
    continue
  fi
  printf 'local secret file may contain only DEEPSEEK_API_KEY=<non-whitespace-value> or GLM_API_KEY=<non-whitespace-value>\n' >&2
  exit 2
done < "$krw_env_file"

if [[ "$krw_env_seen" != true || -z "$krw_env_key" ]]; then
  printf 'local secret file has no usable DEEPSEEK_API_KEY\n' >&2
  exit 2
fi

if [[ "$1" == cargo ]] && ! command -v cargo >/dev/null 2>&1; then
  krw_env_cargo=''
  for krw_env_candidate in "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" /opt/homebrew/opt/rustup/bin/cargo; do
    if [[ -x "$krw_env_candidate" ]]; then
      krw_env_cargo="$krw_env_candidate"
      break
    fi
  done
  if [[ -z "$krw_env_cargo" ]]; then
    printf 'cargo was not found; install Rust or set PATH before running this command\n' >&2
    exit 127
  fi
  krw_env_cargo_dir=$(dirname -- "$krw_env_cargo")
  export PATH="$krw_env_cargo_dir:$PATH"
  set -- "$krw_env_cargo" "${@:2}"
  unset krw_env_cargo krw_env_cargo_dir krw_env_candidate
fi

export DEEPSEEK_API_KEY="$krw_env_key"
if [[ "$krw_env_glm_seen" == true && -n "$krw_env_glm_key" ]]; then
  export GLM_API_KEY="$krw_env_glm_key"
fi
unset krw_env_key krw_env_glm_key krw_env_seen krw_env_glm_seen
exec "$@"
