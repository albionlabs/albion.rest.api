#!/bin/bash
set -euxo pipefail

export COMMIT_SHA=$(git rev-parse HEAD)

# Initialize and update all submodules
echo "Initializing all submodules..."
git submodule update --init --recursive

echo "Preparing orderbook Solidity artifacts..."
nix run .#prepSolArtifacts

echo "Building docs..."
nix build .#albion-docs
rm -rf docs/book
cp -r result docs/book

echo "Setup complete!"
