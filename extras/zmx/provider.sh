#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

zmx_record() {
  local target records field candidate pid created
  target=$1
  if ! records=$(zmx list); then
    printf 'orc-provider-zmx: cannot inspect Zmx sessions\n' >&2
    return 2
  fi
  while IFS=$'\t' read -r -a fields; do
    candidate=
    pid=
    created=
    for field in "${fields[@]}"; do
      field=${field#"${field%%[![:space:]]*}"}
      case "$field" in
        *name=*) candidate=${field#*name=} ;;
        pid=*) pid=${field#pid=} ;;
        created=*) created=${field#created=} ;;
      esac
    done
    if [[ ($candidate == "$target" || $candidate == "${ZMX_SESSION_PREFIX:-}$target") && -n $pid && -n $created ]]; then
      printf '%s\t%s\t%s\n' "$pid" "$created" "$candidate"
      return 0
    fi
  done <<< "$records"
  return 1
}

if [[ ${1:-} == stop ]]; then
  name=${2:?zmx session name is required}
  expected_pid=${3:?zmx session pid is required}
  expected_created=${4:?zmx session birth time is required}
  : "${5:?Orc operation ID is required}"
  if ! command -v zmx > /dev/null; then
    printf 'orc-provider-zmx: zmx is unavailable while stopping %s\n' "$name" >&2
    exit 2
  fi
  record_status=0
  record=$(zmx_record "$name") || record_status=$?
  if ((record_status == 2)); then
    exit 2
  fi
  if ((record_status == 1)); then
    exit 0
  fi
  IFS=$'\t' read -r current_pid current_created current_name <<< "$record"
  if [[ $current_pid != "$expected_pid" || $current_created != "$expected_created" ]]; then
    exit 0
  fi
  ZMX_SESSION_PREFIX='' zmx kill "$current_name"
  exit 0
fi

provider_init "zmx"

case "$capability" in
  provider.validate)
    validate_manifest_requirements
    ;;
  session.bind)
    existing_ref=$(jq -r '
      first(
        .session.providers[]?
        | select(.provider == "zmx" and .kind == "persistence" and .status == "active")
        | .ref
      ) // empty
    ' <<< "$request")
    rebind_current=$(jq -r '.rebindCurrent // false' <<< "$request")
    existing_session=false
    existing_created=${existing_ref##*@}
    existing_identity=${existing_ref%@*}
    existing_pid=${existing_identity##*@}
    existing_name=${existing_identity%@*}
    if [[ $existing_name == "$existing_identity" ]]; then
      existing_name=$existing_ref
      existing_pid=
      existing_created=
    fi
    if [[ -n $existing_ref ]] && command -v zmx > /dev/null; then
      record_status=0
      record=$(zmx_record "$existing_name") || record_status=$?
      if ((record_status == 2)); then
        exit 2
      fi
      if ((record_status == 0)); then
        IFS=$'\t' read -r current_pid current_created current_name <<< "$record"
      fi
      if ((record_status == 0)) && [[ -n $current_pid && (-z $existing_pid || ($current_pid == "$existing_pid" && $current_created == "$existing_created")) ]]; then
        existing_session=true
      fi
    fi
    if [[ $rebind_current == true ]] && current_session_matches && [[ -n ${ZMX_SESSION:-} ]]; then
      zmx_session=$ZMX_SESSION
      record_status=0
      record=$(zmx_record "$zmx_session") || record_status=$?
      if ((record_status == 2)); then
        exit 2
      fi
      if ((record_status == 0)); then
        IFS=$'\t' read -r current_pid current_created current_name <<< "$record"
        emit_binding "persistence" "active" "$current_name@$current_pid@$current_created" "Zmx session $current_name"
      else
        emit_binding "persistence" "available" "" "Zmx on next launch"
      fi
    elif [[ $existing_session == true ]]; then
      emit_binding "persistence" "active" "$current_name@$current_pid@$current_created" "Zmx session $current_name"
    else
      managed_name=$(jq -r '.session.id // empty' <<< "$request")
      record_status=1
      if [[ -n $managed_name ]]; then
        record_status=0
        record=$(zmx_record "$managed_name") || record_status=$?
        if ((record_status == 2)); then
          exit 2
        fi
      fi
      if ((record_status == 0)); then
        IFS=$'\t' read -r current_pid current_created current_name <<< "$record"
        emit_binding "persistence" "active" "$current_name@$current_pid@$current_created" "Zmx session $current_name"
      else
        emit_binding "persistence" "available" "" "Zmx on next launch"
      fi
    fi
    ;;
  session.persist)
    executable=$(command -v zmx || true)
    if [[ -z $executable ]]; then
      emit_declined "zmx is unavailable"
      exit 0
    fi
    provider_ref=$(jq -r '
      if .action == "launch" then empty
      else
        first(
          .session.providers[]?
          | select(.provider == "zmx" and .kind == "persistence" and .status == "active")
          | .ref
        ) // .session.providerRef // empty
      end
    ' <<< "$request")
    if [[ -n $provider_ref ]]; then
      provider_identity=${provider_ref%@*}
      provider_name=${provider_identity%@*}
      if [[ $provider_name == "$provider_identity" ]]; then
        provider_name=$provider_ref
      fi
      emit_plan "$scope" '{}' env ZMX_SESSION_PREFIX= "$executable" attach "$provider_name"
    else
      plan_command=()
      read_command '.plan.command' plan_command
      managed_id=$(jq -r '.session.id' <<< "$request")
      managed_id=${managed_id//[^[:alnum:]_.-]/-}
      prior_environment=$(jq -ce '.plan.environment // {}' <<< "$request")
      emit_plan "$scope" "$prior_environment" env ZMX_SESSION_PREFIX= "$executable" attach "$managed_id" "${plan_command[@]}"
    fi
    ;;
  session.stop)
    executable=$(command -v zmx || true)
    if [[ -z $executable ]]; then
      emit_declined "zmx is unavailable"
      exit 0
    fi
    provider_ref=$(jq -r '
      first(
        .session.providers[]?
        | select(.provider == "zmx" and .kind == "persistence" and .ref != null)
        | .ref
      ) // .session.providerRef // empty
    ' <<< "$request")
    if [[ -z $provider_ref ]]; then
      emit_declined "session has no persistent process reference"
      exit 0
    fi
    operation_id=$(jq -er '.operationId' <<< "$request")
    if [[ ! $operation_id =~ ^[[:alnum:]_.-]+$ ]]; then
      printf 'orc-provider-zmx: invalid operation ID\n' >&2
      exit 2
    fi
    operation_directory=${XDG_STATE_HOME:-$HOME/.local/state}/orc/providers/zmx/operations
    session_id=$(jq -er '.session.id' <<< "$request")
    session_key=$(jq -nr --arg value "$session_id" '$value | @uri')
    operation_path=$operation_directory/$session_key
    if [[ -s $operation_path ]]; then
      IFS=$'\t' read -r recorded_operation provider_name provider_pid provider_created < "$operation_path"
    fi
    if [[ ${recorded_operation:-} != "$operation_id" ]]; then
      provider_created=${provider_ref##*@}
      provider_identity=${provider_ref%@*}
      provider_pid=${provider_identity##*@}
      provider_name=${provider_identity%@*}
      if [[ $provider_name == "$provider_identity" ]]; then
        record_status=0
        record=$(zmx_record "$provider_ref") || record_status=$?
        if ((record_status == 2)); then
          exit 2
        fi
        if ((record_status == 1)); then
          emit_plan "$scope" '{}' true
          exit 0
        fi
        IFS=$'\t' read -r provider_pid provider_created provider_name <<< "$record"
      fi
      umask 077
      mkdir -p "$operation_directory"
      temporary=$operation_path.$$.tmp
      printf '%s\t%s\t%s\t%s\n' "$operation_id" "$provider_name" "$provider_pid" "$provider_created" > "$temporary"
      mv "$temporary" "$operation_path"
    fi
    provider_self=${ORC_PROVIDER_SELF:-$0}
    emit_plan "$scope" '{}' "$provider_self" stop "$provider_name" "$provider_pid" "$provider_created" "$operation_id"
    ;;
  *) unsupported_capability ;;
esac
