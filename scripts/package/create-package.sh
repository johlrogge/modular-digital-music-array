#!/usr/bin/env bash
# Create Void package for beacon (everything comes from void-packages/srcpkgs/beacon/)
exec "$(dirname "${BASH_SOURCE[0]}")/create-service-package.sh" --srcpkg void-packages/srcpkgs/beacon
