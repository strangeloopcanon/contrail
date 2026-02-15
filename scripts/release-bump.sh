#!/usr/bin/env bash

set -euo pipefail

USAGE="Usage: ./scripts/release-bump.sh [patch|minor|major]"

if [[ ${1:-} == "-h" || ${1:-} == "--help" ]]; then
    echo "$USAGE"
    exit 0
fi

BUMP_TYPE="${1:-patch}"
if [[ "$BUMP_TYPE" != "patch" && "$BUMP_TYPE" != "minor" && "$BUMP_TYPE" != "major" ]]; then
    echo "$USAGE" >&2
    exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION_MAP="$(mktemp)"

trap 'rm -f "$VERSION_MAP"' EXIT

manifest_version() {
    awk -F '"' '/^\[package\]/{in_pkg=1; next} in_pkg && /^version = / {print $2; exit} /^\[/{if(in_pkg) exit}' "$1"
}

bump_version() {
    local version="$1"
    local major
    local minor
    local patch

    if [[ ! "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
        echo "Invalid version: $version" >&2
        exit 1
    fi

    major="${BASH_REMATCH[1]}"
    minor="${BASH_REMATCH[2]}"
    patch="${BASH_REMATCH[3]}"

    case "$BUMP_TYPE" in
        major)
            major=$((major + 1))
            minor=0
            patch=0
            ;;
        minor)
            minor=$((minor + 1))
            patch=0
            ;;
        patch)
            patch=$((patch + 1))
            ;;
    esac

    printf "%s.%s.%s\n" "$major" "$minor" "$patch"
}

set_package_version() {
    local manifest="$1"
    local new_version="$2"
    local tmp_file

    tmp_file="$(mktemp)"
    awk -v new_version="$new_version" '
        /^\[package\]/ { in_package=1 }
        in_package && /^version = / {
            sub(/"[^"]+"/, "\"" new_version "\"")
            done=1
        }
        in_package && /^\[/ && $0 != "[package]" { in_package=0 }
        { print }
    ' "$manifest" > "$tmp_file"
    mv "$tmp_file" "$manifest"
}

set_dependency_version() {
    local manifest="$1"
    local dependency="$2"
    local new_version="$3"
    local tmp_file

    if ! grep -Eq "^[[:space:]]*${dependency}[[:space:]]*=" "$manifest"; then
        return
    fi

    tmp_file="$(mktemp)"
    while IFS= read -r line; do
        if [[ "$line" =~ ^[[:space:]]*${dependency}[[:space:]]*=.*\{.*\}.*$ ]]; then
            local comment=""
            if [[ "$line" == *"#"* ]]; then
                comment="#${line#*#}"
                line="${line%%#*}"
            fi

            if [[ "$line" == *"version ="* ]]; then
                line="$(printf '%s' "$line" | perl -pe "s/(version[[:space:]]*=[[:space:]]*\")[0-9]+\\.[0-9]+\\.[0-9]+(\")/\\1${new_version}\\2/")"
            else
                line="$(printf '%s' "$line" | perl -pe "s/\\}\\s*$/, version = \\\"${new_version}\\\"}/")"
            fi

            line="${line}${comment}"
        fi
        printf '%s\n' "$line" >> "$tmp_file"
    done < "$manifest"
    mv "$tmp_file" "$manifest"
}

bump_and_store() {
    local manifest="$1"
    local package_name="$2"

    local manifest_path="$ROOT_DIR/$manifest"
    local current_version
    local next_version

    current_version="$(manifest_version "$manifest_path")"
    next_version="$(bump_version "$current_version")"

    set_package_version "$manifest_path" "$next_version"
    echo "$package_name|$next_version" >> "$VERSION_MAP"
}

get_new_version() {
    local package_name="$1"
    awk -F '|' -v pkg="$package_name" '$1 == pkg {print $2; exit}' "$VERSION_MAP"
}

bump_and_store "contrail_types/Cargo.toml" "contrail-types"
bump_and_store "scrapers/Cargo.toml" "scrapers"
bump_and_store "importer/Cargo.toml" "importer"
bump_and_store "core_daemon/Cargo.toml" "core_daemon"
bump_and_store "dashboard/Cargo.toml" "dashboard"
bump_and_store "analysis/Cargo.toml" "analysis"
bump_and_store "tools/exporter/Cargo.toml" "exporter"
bump_and_store "tools/wrapup/Cargo.toml" "wrapup"
bump_and_store "tools/memex/Cargo.toml" "contrail-memex"
bump_and_store "tools/contrail/Cargo.toml" "contrail-cli"

set_dependency_version "$ROOT_DIR/core_daemon/Cargo.toml" "scrapers" "$(get_new_version "scrapers")"
set_dependency_version "$ROOT_DIR/dashboard/Cargo.toml" "scrapers" "$(get_new_version "scrapers")"
set_dependency_version "$ROOT_DIR/analysis/Cargo.toml" "scrapers" "$(get_new_version "scrapers")"
set_dependency_version "$ROOT_DIR/analysis/Cargo.toml" "contrail-types" "$(get_new_version "contrail-types")"
set_dependency_version "$ROOT_DIR/scrapers/Cargo.toml" "contrail-types" "$(get_new_version "contrail-types")"
set_dependency_version "$ROOT_DIR/importer/Cargo.toml" "scrapers" "$(get_new_version "scrapers")"
set_dependency_version "$ROOT_DIR/tools/memex/Cargo.toml" "scrapers" "$(get_new_version "scrapers")"
set_dependency_version "$ROOT_DIR/tools/contrail/Cargo.toml" "importer" "$(get_new_version "importer")"
set_dependency_version "$ROOT_DIR/tools/wrapup/Cargo.toml" "contrail-types" "$(get_new_version "contrail-types")"

echo "Updated versions:"
sort "$VERSION_MAP" | while IFS="|" read -r package version; do
    printf "  %s => %s\n" "$package" "$version"
done
