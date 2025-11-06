#!/usr/bin/env bash
set -euo pipefail

# This script patches an issue between the window_manager and compositor crates.
#
# compositor uses libcef, which is packaged with CEF (Chromium).
# A pre-built version of CEF is downloaded, which comes with a set of libraries.
# Including a version of libvulkan that misses some extensions we require
# ("VK_KHR_xlib_surface" to display window in X11 environements)
# On Linux, it is found on the target folder, which is included in LD_LIBRARY_PATH
# (/target/<profile>/lib/libvulkan.so.1). This causes it to be linked rather then
# the host full version of the library.
#
# This patch simply removes the library to make sure it does not get loaded.
# It needs to be run after process_helper or compositor is built.

# Configuration
LIB_NAME="libvulkan.so.1"

# Remove library
found_any=false

shopt -s nullglob
for lib_path in ./target/*/lib/${LIB_NAME}*; do
  rm -f "${lib_path}"
  echo "Removed: ${lib_path}"
  found_any=true
done
shopt -u nullglob

if [[ "${found_any}" == false ]]; then
  echo "Skipping, no bundled '${LIB_NAME}' found in any ./target/*/lib/ directory."
fi
