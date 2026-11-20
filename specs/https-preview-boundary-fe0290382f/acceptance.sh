set -eu
build=${CARGO_TARGET_DIR:-target}
cargo build --quiet --locked --lib --no-default-features
temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
rustc --edition=2024 "$1/acceptance.rs" --extern "aep=$build/debug/libaep.rlib" \
    -L "dependency=$build/debug/deps" -o "$temporary/check"
"$temporary/check" "$temporary/run"
printf '%s\n' 'Passed AC-1 and AC-2 for the Studio UTF-8 preview'
