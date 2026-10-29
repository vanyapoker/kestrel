#!/usr/bin/env bash
# Build the local Linux x86_64 release and package its binary, installer and assets.
set -euo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"
if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
    echo "This packager supports native Linux x86_64 builds." >&2
    exit 1
fi

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml)"
package="kestrel-$version-linux-x86_64"
output_dir="$repo_dir/target/releases"
cargo build --release --locked --target-dir "$repo_dir/target"
binary="$repo_dir/target/release/kestrel"
test "$("$binary" --version)" = "kestrel $version"

mkdir -p "$output_dir"
staging_dir="$(mktemp -d "$output_dir/.package.XXXXXX")"
trap 'rm -rf -- "$staging_dir"' EXIT
bundle="$staging_dir/$package"
install -Dm755 "$binary" "$bundle/kestrel"
install -Dm755 scripts/install-prebuilt.sh "$bundle/install.sh"
install -Dm755 scripts/integrate.sh "$bundle/scripts/integrate.sh"
install -Dm644 assets/kestrel.desktop "$bundle/assets/kestrel.desktop"
install -Dm644 assets/kestrel.svg "$bundle/assets/kestrel.svg"
install -Dm644 LICENSE "$bundle/LICENSE"
install -Dm644 README.md "$bundle/README.md"
printf '%s\n' "$version" > "$bundle/VERSION"
{
    printf 'Kestrel %s\nTarget: Linux x86_64 (GNU libc)\n' "$version"
    rustc --version
    printf 'Required glibc symbol versions:\n'
    readelf --version-info "$binary" | sed -n 's/.*Name: \(GLIBC_[0-9.]*\).*/\1/p' | sort -Vu
} > "$bundle/BUILD-INFO.txt"

tar --sort=name --owner=0 --group=0 --numeric-owner -czf "$output_dir/$package.tar.gz" -C "$staging_dir" "$package"
(
    cd "$output_dir"
    sha256sum "$package.tar.gz" > SHA256SUMS
    sha256sum --check SHA256SUMS
)
printf 'Release assets: %s/%s.tar.gz and %s/SHA256SUMS\n' "$output_dir" "$package" "$output_dir"
