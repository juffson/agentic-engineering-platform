#!/bin/sh
# Run the AEP permission hook with Node from PATH, or with ZCode's own runtime.
dir=$(dirname "$0")
if command -v node >/dev/null 2>&1; then
  exec node "$dir/permission.mjs"
fi
for app in "/Applications/ZCode.app" "$HOME/Applications/ZCode.app"; do
  if [ -x "$app/Contents/MacOS/ZCode" ]; then
    ELECTRON_RUN_AS_NODE=1 exec "$app/Contents/MacOS/ZCode" "$dir/permission.mjs"
  fi
done
# No runtime: say nothing, so ZCode asks as usual.
exit 0
