#!/bin/sh
set -e

BINARY="krun-build-image"
BUILD="target/release/$BINARY"
DIST="dist"
LIBS="$DIST/libs"

if ! command -v dylibbundler >/dev/null 2>&1; then
    echo "dylibbundler not found. Install with: brew install dylibbundler"
    exit 1
fi

# Release build
cargo build --release

rm -rf "$DIST"
mkdir -p "$LIBS"
# The actual binary is krun-build-image.bin; krun-build-image is a wrapper script that
# sets DYLD_LIBRARY_PATH so dlopen can find libkrunfw.
cp "$BUILD" "$DIST/$BINARY.bin"

# Bundle static dylib dependencies and rewrite their load paths.
# Must happen before signing — path rewrites invalidate any existing signature.
DYLD_LIBRARY_PATH="$(brew --prefix)/lib:${DYLD_LIBRARY_PATH}" \
dylibbundler -b \
    -x "$DIST/$BINARY.bin" \
    -d "$LIBS" \
    -p @executable_path/libs/ \
    -od

# libkrunfw is loaded at runtime via dlopen() inside libkrun, so dylibbundler
# won't see it as a static dependency. Copy and fix it manually.
KRUNFW=$(ls "$(brew --prefix)/lib/libkrunfw"*.dylib 2>/dev/null | head -1)
if [ -z "$KRUNFW" ]; then
    echo "libkrunfw not found under $(brew --prefix)/lib — is libkrun/krun/libkrun installed?"
    exit 1
fi
BASENAME=$(basename "$KRUNFW")
cp "$KRUNFW" "$LIBS/"
# Keep the install name as the bare filename (e.g. "libkrunfw.5.dylib").
# Our preload_krunfw() loads it by full path; dyld then registers it under
# this install name. When libkrun calls dlopen("libkrunfw.5.dylib"), dyld
# matches by install name and returns the already-loaded handle.
install_name_tool -id "$BASENAME" "$LIBS/$BASENAME"

# Sign everything. Use your Developer ID certificate instead of '-' for
# notarized distribution: --sign "Developer ID Application: Name (TEAMID)"
# With a real cert, also add --options runtime for hardened runtime.
for lib in "$LIBS"/*.dylib; do
    codesign --sign - --force "$lib"
done
codesign --sign - --entitlements entitlements.plist --force "$DIST/$BINARY.bin"

# Wrapper script: sets DYLD_LIBRARY_PATH for the dlopen'd libkrunfw.
cat > "$DIST/$BINARY" << 'EOF'
#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
DYLD_LIBRARY_PATH="$DIR/libs:${DYLD_LIBRARY_PATH}" "$DIR/krun-build-image.bin" "$@"
EOF
chmod +x "$DIST/$BINARY"

echo ""
echo "Distribution package ready in $DIST/"
echo "Run with: $DIST/$BINARY <rootfs-path>"
