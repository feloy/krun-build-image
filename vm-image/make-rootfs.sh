#!/bin/sh
# Build and export the libkrun build-VM rootfs to a local directory.
# Requires Podman. The resulting rootfs has buildah installed and is
# pre-configured to use the vfs storage driver (required on virtiofs).
#
# Usage: ./make-rootfs.sh [OUTPUT_DIR]
#   OUTPUT_DIR  where to write the rootfs  (default: /tmp/krun-build-rootfs)
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOTFS="${1:-/tmp/krun-build-rootfs}"
IMAGE="krun-build-rootfs"

if ! command -v podman >/dev/null 2>&1; then
    echo "error: podman is required to build the rootfs" >&2
    exit 1
fi

echo "Building rootfs image for linux/arm64..."
podman build \
    --platform linux/arm64 \
    --tag "$IMAGE" \
    --file "$SCRIPT_DIR/Containerfile" \
    "$SCRIPT_DIR"

echo "Exporting rootfs to $ROOTFS..."
mkdir -p "$ROOTFS"
CONTAINER="$(podman create --platform linux/arm64 "$IMAGE")"
podman export "$CONTAINER" | tar -C "$ROOTFS" -x
podman rm "$CONTAINER" >/dev/null

# podman create may mount the host's /etc/resolv.conf into the container,
# overwriting the one baked into the image. Write it explicitly after export.
# Use-vc forces TCP for DNS queries — TSI routes TCP but may not route UDP.
printf 'nameserver 1.1.1.1\noptions use-vc\n' > "$ROOTFS/etc/resolv.conf"

echo ""
echo "Rootfs ready at: $ROOTFS"
echo ""
echo "Build an OCI image:"
echo "  ./run.sh --rootfs $ROOTFS -t myimage:latest ./my-context"
