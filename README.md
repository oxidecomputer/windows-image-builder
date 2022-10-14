# Windows Image Builder

Scripts and tooling to build a Windows images suitable to use on an Oxide rack.

Specifically, we grab a Windows Server 2022 evaluation ISO and install it to a
raw image as part of an unattended installation.

## Drivers

The VirtIO Block and Net drivers are pre-installed.

## cloud-init

[Cloudbase-init](https://cloudbase-init.readthedocs.io/en/latest/intro.html) is
pre-installed in the image and configured to work with the metadata provided
by the Oxide control plane.

**NOTE**: A [fork](https://github.com/luqmana/cloudbase-init/tree/oxide) is
used to workaround some issues in the upstream version.