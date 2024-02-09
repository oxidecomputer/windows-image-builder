#!/bin/bash

set -eu

FLAVOR_DIR=""

case $(uname -s) in
    SunOS) 
        echo "copying unattend configuration for illumos"
        FLAVOR_DIR="illumos"
        ;;
    Linux) 
        echo "copying unattend configuration for Linux"
        FLAVOR_DIR="linux"
        ;;
    *)
        echo "unsupported host OS $(uname -s)"
        exit 1
        ;;
esac

if [[ ! -d "out" ]]; then
    mkdir out
fi

if [[ ! -d "out/unattend" ]]; then
    mkdir out/unattend
fi

find unattend -maxdepth 1 -type f -exec cp {} out/unattend \; || true
cp unattend/$FLAVOR_DIR/* out/unattend

echo "copied unattend files to \`out/unattend\`"
