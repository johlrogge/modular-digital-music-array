#!/usr/bin/env bash
# Create Void package for mdma-admin (everything comes from void-packages/srcpkgs/mdma-admin/)
exec "$(dirname "${BASH_SOURCE[0]}")/create-service-package.sh" --srcpkg void-packages/srcpkgs/mdma-admin
