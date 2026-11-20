set -eu
probe=tests/aep_https_manifest_path_contract.rs
test ! -e "$probe"
trap 'rm -f -- "$probe"' EXIT
cp "$1/acceptance.rs" "$probe"
# Give the isolated rehearsal time to observe the checking phase.
if test -f /etc/aep-pilot-container; then sleep 10; fi
cargo test --quiet --locked --no-default-features --test aep_https_manifest_path_contract
