#!/usr/bin/env bash
# Capture one frame of Overgeared under RenderDoc (tools/renderdoc-dl).
# usage: ./run-og-rdoc.sh [frame]   (default 1800 = title screen)
# The capture lands in games/overgeared/rdoc/title_frame*.rdc; the game is
# closed once the capture file appears.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
P=com.nexon.overgeared
RD=tools/renderdoc-dl/RenderDoc_1.40_64/renderdoccmd.exe
mkdir -p games/overgeared/rdoc
export WWD_RDOC=${1:-1800}
# the portable build doesn't register its Vulkan layer; point the loader at it
export VK_ADD_LAYER_PATH="$(cygpath -w "$PWD/${RD%/*}")"
export VK_LOADER_LAYERS_ENABLE=VK_LAYER_RENDERDOC_Capture
export ENABLE_VULKAN_RENDERDOC_CAPTURE=1
export WWD_FONT_FIX=${WWD_FONT_FIX:-$(seq -s, 700 60 2300)}
"$RD" capture -d "$(cygpath -w "$PWD")" -c "$(cygpath -w "$PWD")\\games\\overgeared\\rdoc\\title" \
  "$(cygpath -w "$PWD")\\target\\release\\wwd.exe" --root rootfs-android \
  --env LD_LIBRARY_PATH=/wwd/lib64:/data/app/$P/lib/arm64:/system/lib64 \
  rootfs-android/wwd/bin/wwd-app $P Temppal
before=$(ls games/overgeared/rdoc/*.rdc 2>/dev/null | wc -l)
for _ in $(seq 300); do
  [ "$(ls games/overgeared/rdoc/*.rdc 2>/dev/null | wc -l)" -gt "$before" ] && break
  sleep 2
done
sleep 15 # let RenderDoc finish writing
taskkill //F //IM wwd.exe >/dev/null 2>&1
ls -la games/overgeared/rdoc
