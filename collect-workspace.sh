#!/usr/bin/env bash
# Recursively find and output all relevant source files

# Start with root Cargo.toml
echo "---- Cargo.toml"
cat Cargo.toml
echo

# Find and output all component and base source files
find . -type f \( -name "*.rs" -o -name "Cargo.toml" \) \
    -not -path "./target/*" \
    -not -path "./.git/*" \
    | sort \
    | while read -r file; do
        echo "---- ${file#./}"
        cat "$file"
        echo
done