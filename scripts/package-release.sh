#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"

version=$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"name":"javelin","version":"\([^"]*\)".*/\1/p')
if [ -z "$version" ]; then
  echo "cannot determine package version" >&2
  exit 1
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) platform=aarch64-apple-darwin ;;
  Darwin-x86_64) platform=x86_64-apple-darwin ;;
  Linux-x86_64) platform=x86_64-unknown-linux-gnu ;;
  Linux-aarch64) platform=aarch64-unknown-linux-gnu ;;
  *)
    echo "unsupported local packaging platform: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

cargo build --locked --release
rm -rf "dist/javelin-$version-$platform"
mkdir -p "dist/javelin-$version-$platform/completions"
cp target/release/javelin "dist/javelin-$version-$platform/javelin"
cp LICENSE README.md CHANGELOG.md "dist/javelin-$version-$platform/"
for shell in bash elvish fish powershell zsh; do
  target/release/javelin completions "$shell" > "dist/javelin-$version-$platform/completions/javelin.$shell"
done

archive="dist/javelin-$version-$platform.tar.gz"
tar -C dist -czf "$archive" "javelin-$version-$platform"
shasum -a 256 "$archive" > "$archive.sha256"
printf '%s\n' "$archive"
printf '%s\n' "$archive.sha256"
