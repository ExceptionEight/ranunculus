#!/usr/bin/env bash
# Prints the version of this tree, having checked it is the same everywhere it is written: the Rust workspace, the web
# UI and the images compose.yaml names; and that the server's image builds with the Rust of rust-toolchain.toml.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || { echo "version.sh: no version in Cargo.toml" >&2; exit 1; }

web=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' web/package.json)
[ "$web" = "$version" ] || { echo "version.sh: web/package.json says $web, Cargo.toml $version" >&2; exit 1; }

others=$(grep -o 'ghcr.io/exceptioneight/ranunculus-[a-z]*:[^ ]*' compose.yaml | grep -v ":$version\$" || true)
[ -z "$others" ] || { echo "version.sh: compose.yaml names other versions than $version: $others" >&2; exit 1; }

rust=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
images=$(grep -o 'FROM rust:[^ ]*' server/Dockerfile | grep -v "FROM rust:$rust-slim\$" | sort -u | tr '\n' ' ' || true)
[ -z "$images" ] || { echo "version.sh: server/Dockerfile builds with $images, rust-toolchain.toml says $rust" >&2; exit 1; }

echo "$version"
