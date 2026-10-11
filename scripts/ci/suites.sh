# shellcheck shell=bash
# Discover CI example suites from ict-ci.rs (single SoT). Sourced by pack/curate.
ict_ci_src() {
  echo "${ICT_RS_ROOT:?}/ict-rs/src/bin/ict-ci.rs"
}

# Print a rust `const NAME: ... = &[...];` block (one-line or multi-line).
_ict_ci_const_block() {
  awk -v n="$1" '
    $0 ~ "const " n {grab=1}
    grab {print}
    grab && /;$/ {exit}
  ' "$(ict_ci_src)"
}

_ict_ci_quoted_idents() {
  grep -o '"[a-z0-9_]*"' | tr -d '"'
}

# Prints suite names, one per line (mock then docker, as declared).
ict_ci_suites() {
  { _ict_ci_const_block MOCK_SUITES; _ict_ci_const_block DOCKER_SUITES; } \
    | _ict_ci_quoted_idents
}

ict_ci_suites_mock() {
  _ict_ci_const_block MOCK_SUITES | _ict_ci_quoted_idents
}

ict_ci_suites_docker() {
  _ict_ci_const_block DOCKER_SUITES | _ict_ci_quoted_idents
}

# Fail if a dispatched suite is not an [[example]] in ict-rs/Cargo.toml.
ict_ci_assert_examples_declared() {
  local toml="${ICT_RS_ROOT}/ict-rs/Cargo.toml"
  local s
  while IFS= read -r s; do
    [ -n "$s" ] || continue
    if ! grep -q "^name = \"${s}\"" "$toml"; then
      echo "suite ${s} not declared as [[example]] in ict-rs/Cargo.toml" >&2
      return 1
    fi
  done < <(ict_ci_suites)
}

ict_ci_example_flags() {
  local s flags=()
  while IFS= read -r s; do
    [ -n "$s" ] || continue
    flags+=(--example "$s")
  done < <(ict_ci_suites)
  printf '%s\n' "${flags[@]}"
}
