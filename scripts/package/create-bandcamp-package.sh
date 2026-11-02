#!/usr/bin/env bash
# Create Void package for mdma-bandcamp (everything comes from void-packages/srcpkgs/mdma-bandcamp/)
exec "$(dirname "${BASH_SOURCE[0]}")/create-service-package.sh" --srcpkg void-packages/srcpkgs/mdma-bandcamp
