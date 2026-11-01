#!/bin/sh
# Rebuild the bundled test plugins and fetch sdpi-components (see README.md).
#   sh tests/fixtures/elgato-sdk/build.sh
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for v in 1 2 3; do
    mkdir -p "$tmp/v$v"
    cp "$here/plugin-src.mjs" "$tmp/v$v/"
    (cd "$tmp/v$v" && npm init -y >/dev/null && npm install --silent "@elgato/streamdeck@$v" >/dev/null)
    (cd "$tmp/v$v" && esbuild plugin-src.mjs --bundle --minify --platform=node --format=esm --target=node20 \
        --define:LIB_MAJOR=$v --legal-comments=eof --log-level=warning --outfile="$here/plugin-v$v.mjs" \
        --banner:js="import { createRequire as __cr } from 'node:module'; const require = __cr(import.meta.url);")
    echo "plugin-v$v.mjs: @elgato/streamdeck $(node -p "require('$tmp/v$v/node_modules/@elgato/streamdeck/package.json').version")"
done
curl -sSfL -o "$here/sdpi-components.js" https://sdpi-components.dev/releases/v4/sdpi-components.js
head -3 "$here/sdpi-components.js" | sed -n 3p
