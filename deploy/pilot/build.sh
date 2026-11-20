#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "$0")/../.."
export COPYFILE_DISABLE=1
rust_image=${AEP_PILOT_RUST_IMAGE:-rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea}
runtime_image=${AEP_PILOT_HELPER_IMAGE:-debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc}
node_image=${AEP_PILOT_NODE_IMAGE:-node:24-bookworm-slim@sha256:dc26ae92c9fb15cf6b1d63e20f0944b2efe0f115b20e00be48fb23fceb061c54}
for target in control worker; do
    if [[ $target == control ]]; then
        image=${AEP_PILOT_CONTROL_IMAGE:-aep-pilot-control:trial}
    else
        image=${AEP_PILOT_WORKER_IMAGE:-aep-pilot-worker:trial}
    fi
    tar --no-xattrs -cf - Cargo.toml Cargo.lock src deploy/pilot | \
        docker build --target "$target" --tag "$image" -f deploy/pilot/Dockerfile \
            --build-arg "RUST_IMAGE=$rust_image" --build-arg "RUNTIME_IMAGE=$runtime_image" \
            --build-arg "NODE_IMAGE=$node_image" -
done
tar --no-xattrs -cf - deploy/pilot/egress.Dockerfile deploy/pilot/squid.conf | \
    docker build --tag "${AEP_PILOT_EGRESS_IMAGE:-aep-pilot-egress:trial}" \
        --build-arg "RUNTIME_IMAGE=$runtime_image" -f deploy/pilot/egress.Dockerfile -
