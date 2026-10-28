#!/usr/bin/env bash
# Line coverage gate. Exclusions are thin OS adapters that tests cannot drive:
#   os_glue/        launchctl/systemd/registry, native notifications, tray event loop
#   bollard_api.rs  real Docker adapter (covered by SPOKO_DOCKER_TESTS=1 runs)
#   tests/, benches/ test code itself
#   src/main.rs     one-line delegation to the crate's lib run()
set -euo pipefail
cd "$(dirname "$0")/.."
cargo llvm-cov --workspace --exclude spoko-tray \
  --ignore-filename-regex '(os_glue/|bollard_api\.rs|/tests/|/benches/|test_util|src/main\.rs)' \
  --fail-under-lines "${COVERAGE_MIN:-100}" "$@"
