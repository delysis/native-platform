#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

source_repository='https://github.com/delysis/mom-llama.git'
source_main='3cf57941af6d523378e7fa8b24f5c24c8e50363f'
source_tree='7670bc1bfb4b94959871d33f7487d3969b2a76c7'
prefix='products/mom'
filtered_head='8189804d01be5d12384bcd6f01ceb2c7ef2d4fd7'
import_commit='cfa2d3c40e74e1d692c0cdb9354cc272249fd4ab'
import_parent='bc1c6cafe67d5cdbf2441c7155b89f129e8ba730'
cutover_commit='5b12072e91dc44f2f93f6dfc0b869d3cc58c26f1'
workspace_commit='7ad6a0080463ffc9318e48c4b2378fbadd016df8'
lock_commit='a9f0603782b5ad796fc755b6a0dcfe104f9fad38'
map='migration/mom-llama.commit-map'
map_sha256='26a8f595de2879aadca81a937df6e3700b287a92fed43af78e6bfc35050e284c'
commit_count=59

if command -v shasum >/dev/null 2>&1; then
  actual_map_sha256="$(shasum -a 256 "$map" | awk '{print $1}')"
else
  actual_map_sha256="$(sha256sum "$map" | awk '{print $1}')"
fi

test "$actual_map_sha256" = "$map_sha256"
test "$(wc -l < "$map" | tr -d ' ')" = "$((commit_count + 1))"
test "$(git rev-list --count "$filtered_head")" = "$commit_count"
test "$(git rev-parse "${filtered_head}:${prefix}")" = "$source_tree"
test "$(git show -s --format=%P "$import_commit")" = "$import_parent $filtered_head"
git merge-base --is-ancestor "$import_commit" HEAD
git merge-base --is-ancestor "$cutover_commit" HEAD
git merge-base --is-ancestor "$workspace_commit" HEAD
git merge-base --is-ancestor "$lock_commit" HEAD

git fetch --no-tags "$source_repository" "$source_main"
test "$(git rev-parse FETCH_HEAD)" = "$source_main"
test "$(git rev-parse "${source_main}^{tree}")" = "$source_tree"
test "$(git rev-list --count "$source_main")" = "$commit_count"

mapped_count=0
while read -r old new; do
  if [[ "$old" == old ]]; then
    continue
  fi
  test "$new" != 0000000000000000000000000000000000000000
  test "$(git rev-parse "${old}^{tree}")" = "$(git rev-parse "${new}:${prefix}")"

  expected_parents=''
  for old_parent in $(git show -s --format=%P "$old"); do
    mapped_parent="$(awk -v old="$old_parent" '$1 == old { print $2 }' "$map")"
    test -n "$mapped_parent"
    if test -n "$expected_parents"; then
      expected_parents="$expected_parents $mapped_parent"
    else
      expected_parents="$mapped_parent"
    fi
  done
  test "$(git show -s --format=%P "$new")" = "$expected_parents"

  old_identity="$(git show -s --format='%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%s' "$old")"
  new_identity="$(git show -s --format='%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%s' "$new")"
  test "$old_identity" = "$new_identity"
  mapped_count=$((mapped_count + 1))
done < "$map"
test "$mapped_count" = "$commit_count"

echo 'Mom import history: 59 commits preserve topology, identity, dates, subjects, and byte-identical prefixed trees'
