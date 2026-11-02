#!/usr/bin/env bash
# Create Void package for mdma-audio (everything comes from void-packages/srcpkgs/mdma-audio/)
exec "$(dirname "${BASH_SOURCE[0]}")/create-service-package.sh" --srcpkg void-packages/srcpkgs/mdma-audio
