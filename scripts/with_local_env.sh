#!/usr/bin/env bash
set -euo pipefail

krw_env_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_env_file="$krw_env_root/.env.local"

krw_env_scope=agent
if [[ "${1:-}" == "--market-sidecar" ]]; then
  krw_env_scope=market_sidecar
  shift
fi
krw_env_inherited_fmp_key=${FMP_API_KEY:-}
krw_env_inherited_market_store_url=${KRW_MARKET_SNAPSHOT_STORE_URL:-}
krw_env_inherited_market_store_key=${KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY:-}
# Strip ambient secrets before selecting the role-specific child scope below.
# This prevents a developer shell's market credentials from leaking into
# agentd, image-build, or provider processes.
unset GLM_API_KEY DEEPSEEK_API_KEY FMP_API_KEY \
  KRW_MARKET_SNAPSHOT_STORE_URL KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY

if [[ $# -eq 0 ]]; then
  printf 'usage: %s [--market-sidecar] command [argument ...]\n' "$0" >&2
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

krw_env_glm_key=''
krw_env_glm_seen=false
krw_env_fmp_key=''
krw_env_fmp_seen=false
krw_env_market_store_url=''
krw_env_market_store_url_seen=false
krw_env_market_store_key=''
krw_env_market_store_key_seen=false
while IFS= read -r krw_env_line || [[ -n "$krw_env_line" ]]; do
  if [[ -z "$krw_env_line" || "$krw_env_line" == \#* ]]; then
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
  if [[ "$krw_env_line" =~ ^FMP_API_KEY=([^[:space:]#]+)$ ]]; then
    if [[ "$krw_env_fmp_seen" == true ]]; then
      printf 'local secret file defines FMP_API_KEY more than once\n' >&2
      exit 2
    fi
    krw_env_fmp_key=${BASH_REMATCH[1]}
    krw_env_fmp_seen=true
    continue
  fi
  if [[ "$krw_env_line" =~ ^KRW_MARKET_SNAPSHOT_STORE_URL=([^[:space:]#]+)$ ]]; then
    if [[ "$krw_env_market_store_url_seen" == true ]]; then
      printf 'local secret file defines KRW_MARKET_SNAPSHOT_STORE_URL more than once\n' >&2
      exit 2
    fi
    krw_env_market_store_url=${BASH_REMATCH[1]}
    krw_env_market_store_url_seen=true
    continue
  fi
  if [[ "$krw_env_line" =~ ^KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY=([^[:space:]#]+)$ ]]; then
    if [[ "$krw_env_market_store_key_seen" == true ]]; then
      printf 'local secret file defines KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY more than once\n' >&2
      exit 2
    fi
    krw_env_market_store_key=${BASH_REMATCH[1]}
    krw_env_market_store_key_seen=true
    continue
  fi
  # Older local files can retain a DeepSeek key from a previous provider
  # configuration. Keep that file format compatible, but deliberately do not
  # read or export it: this runtime launches GLM only.
  if [[ "$krw_env_line" =~ ^DEEPSEEK_API_KEY=([^[:space:]#]+)$ ]]; then
    continue
  fi
  printf 'local secret file may contain GLM_API_KEY, optional FMP/store credentials, and an ignored legacy DEEPSEEK_API_KEY only\n' >&2
  exit 2
done < "$krw_env_file"

if [[ "$krw_env_glm_seen" != true || -z "$krw_env_glm_key" ]]; then
  printf 'local secret file has no usable GLM_API_KEY\n' >&2
  exit 2
fi
if [[ "$krw_env_market_store_url_seen" != "$krw_env_market_store_key_seen" ]]; then
  printf 'local market snapshot store requires both URL and service-role key\n' >&2
  exit 2
fi
if [[ "$krw_env_market_store_url_seen" != true ]] \
  && [[ -n "$krw_env_inherited_market_store_url" || -n "$krw_env_inherited_market_store_key" ]] \
  && [[ -z "$krw_env_inherited_market_store_url" || -z "$krw_env_inherited_market_store_key" ]]; then
  printf 'inherited local market snapshot store requires both URL and service-role key\n' >&2
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

if [[ "$krw_env_scope" == agent && "$krw_env_glm_seen" == true && -n "$krw_env_glm_key" ]]; then
  export GLM_API_KEY="$krw_env_glm_key"
fi
if [[ "$krw_env_scope" == market_sidecar ]]; then
  if [[ "$krw_env_fmp_seen" == true && -n "$krw_env_fmp_key" ]]; then
    export FMP_API_KEY="$krw_env_fmp_key"
  elif [[ -n "$krw_env_inherited_fmp_key" ]]; then
    export FMP_API_KEY="$krw_env_inherited_fmp_key"
  fi
  if [[ "$krw_env_market_store_url_seen" == true ]]; then
    export KRW_MARKET_SNAPSHOT_STORE_URL="$krw_env_market_store_url"
    export KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY="$krw_env_market_store_key"
  elif [[ -n "$krw_env_inherited_market_store_url" ]]; then
    export KRW_MARKET_SNAPSHOT_STORE_URL="$krw_env_inherited_market_store_url"
    export KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY="$krw_env_inherited_market_store_key"
  fi
fi
unset krw_env_scope krw_env_glm_key krw_env_glm_seen krw_env_fmp_key krw_env_fmp_seen \
  krw_env_market_store_url krw_env_market_store_url_seen \
  krw_env_market_store_key krw_env_market_store_key_seen \
  krw_env_inherited_fmp_key krw_env_inherited_market_store_url krw_env_inherited_market_store_key
exec "$@"
