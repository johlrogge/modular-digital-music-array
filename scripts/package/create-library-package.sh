#!/usr/bin/env bash
# Create Void package for mdma-library (everything comes from void-packages/srcpkgs/mdma-library/)
exec "$(dirname "${BASH_SOURCE[0]}")/create-service-package.sh" --srcpkg void-packages/srcpkgs/mdma-library
