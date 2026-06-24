#!/bin/sh
set -e

BINARY="krun-build-image"
BUILD="target/release/$BINARY"
DIST="dist"

# Release build
cargo build --release

rm -rf "$DIST"
mkdir -p "$DIST"
# The actual binary is krun-build-image.bin; krun-build-image is a wrapper script
# that handles one-time setup on first run (quarantine removal, rootfs extraction).
cp "$BUILD" "$DIST/$BINARY.bin"

# Sign everything. Use your Developer ID certificate instead of '-' for
# notarized distribution: --sign "Developer ID Application: Name (TEAMID)"
# With a real cert, also add --options runtime for hardened runtime.
codesign --sign - --entitlements entitlements.plist --force "$DIST/$BINARY.bin"

# Wrapper script: handles one-time setup on first run (quarantine removal,
# rootfs extraction).
cat > "$DIST/$BINARY" << 'EOF'
#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"

if [ ! -f "$DIR/.setup-done" ]; then
    xattr -d com.apple.quarantine "$DIR/krun-build-image.bin" 2>/dev/null || true
    if [ -f "$DIR/rootfs.zip" ]; then
        unzip -q "$DIR/rootfs.zip" -d "$DIR"
    fi
    touch "$DIR/.setup-done"
fi

"$DIR/krun-build-image.bin" "$@"
EOF
chmod +x "$DIST/$BINARY"

echo ""
echo "Distribution package ready in $DIST/"
echo "Run with: $DIST/$BINARY <context>"
