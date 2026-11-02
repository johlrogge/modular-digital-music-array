#!/usr/bin/env bash
# Create Void package for mdma-acid (everything comes from void-packages/srcpkgs/mdma-acid/)
# The binary must be pre-built with the production profile (file-backed fact-store).
# See build-all.sh Phase 1f or justfile acid-cross for the build command.
exec "$(dirname "${BASH_SOURCE[0]}")/create-service-package.sh" --srcpkg void-packages/srcpkgs/mdma-acid
