#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
command -v hyperfine >/dev/null || { echo "hyperfine is required: https://github.com/sharkdp/hyperfine" >&2; exit 1; }

corpus="${1:-}"
if [[ -z "$corpus" ]]; then
  corpus=data/linux
  if [[ ! -d "$corpus" ]]; then
    mkdir -p data
    curl -fL https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.tar.xz | tar -xJ -C data
    mv data/linux-6.12 "$corpus"
  fi
fi

cargo build --release --quiet --manifest-path ../Cargo.toml
pakk="$(cd .. && pwd)/target/release/pakk"
name="$(basename "$corpus")"
parent="$(cd "$(dirname "$corpus")" && pwd)"
out="$(pwd)/out"
rm -rf "$out" && mkdir -p "$out"

echo "corpus: $corpus ($(du -sh "$corpus" | cut -f1), $(find "$corpus" -type f | wc -l | tr -d ' ') files)"
cd "$parent"
opts=(--warmup 1 --runs "${RUNS:-5}" --export-markdown)

hyperfine "${opts[@]}" "$out/pack-zip.md" -p "rm -f $out/*.zip" \
  -n "zip -r" "zip -qr $out/sys.zip $name" \
  -n "pakk" "$pakk -q pack $name -o $out/pakk.zip"

hyperfine "${opts[@]}" "$out/pack-tgz.md" -p "rm -f $out/*.tar.gz" \
  -n "tar czf" "tar czf $out/sys.tar.gz $name" \
  -n "pakk" "$pakk -q pack $name -o $out/pakk.tar.gz"

hyperfine "${opts[@]}" "$out/pack-zst.md" -p "rm -f $out/*.tar.zst" \
  -n "tar | zstd -T0" "tar cf - $name | zstd -q -T0 -o $out/sys.tar.zst" \
  -n "pakk" "$pakk -q pack $name -o $out/pakk.tar.zst"

zip -qr "$out/sys.zip" "$name" && "$pakk" -q pack "$name" -o "$out/pakk.tar.gz" --force

hyperfine "${opts[@]}" "$out/unpack-zip.md" -p "rm -rf $out/x" \
  -n "unzip" "unzip -q $out/sys.zip -d $out/x" \
  -n "pakk" "$pakk -q unpack $out/sys.zip -C $out/x"

hyperfine "${opts[@]}" "$out/unpack-tgz.md" -p "rm -rf $out/x && mkdir $out/x" \
  -n "tar xzf" "tar xzf $out/pakk.tar.gz -C $out/x" \
  -n "pakk" "$pakk -q unpack $out/pakk.tar.gz -C $out/x"

echo
ls -l "$out" | awk 'NR>1 && $NF ~ /\.(zip|gz|zst)$/ {printf "%-16s %8.1f MiB\n", $NF, $5/1048576}'
