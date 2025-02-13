#!/bin/bash
set -e

# Check if git-filter-repo is installed
if ! command -v git-filter-repo &> /dev/null; then
    echo "git-filter-repo is not installed. Installing..."
    # You might need to adjust this for your package manager
    pip3 install git-filter-repo
fi

# Ensure we're in a clean state
if [ -n "$(git status --porcelain)" ]; then
    echo "Working directory is not clean. Please commit or stash changes first."
    exit 1
fi

# Create a backup of the current branch name
current_branch=$(git rev-parse --abbrev-ref HEAD)

# Remove the library directory from git history
echo "Removing library directory from git history..."
git filter-repo --force --path 'library/' --invert-paths

# Force push all branches
echo ""
echo "Directory removed from git history. Next steps:"
echo "1. Force push changes to remote with:"
echo "   git push origin --force --all"
echo "   git push origin --force --tags"
echo ""
echo "2. Have all team members:"
echo "   - Delete their local copies"
echo "   - Clone the repository fresh"
echo ""
echo "3. Clean up by running:"
echo "   git for-each-ref --format=\"%(refname)\" refs/original/ | xargs -n 1 git update-ref -d"
echo "   git reflog expire --expire=now --all"
echo "   git gc --aggressive --prune=now"
