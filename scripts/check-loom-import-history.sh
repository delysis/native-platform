#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

source_repository='https://github.com/delysis/loom-native.git'
source_main='223110bee4be72386d79306b444517371e4a9930'
source_tree='89eeaa6129d42d31ebb16b425189b3ffefb16724'
prefix='products/loom'
filtered_head='fe35a14bfad7fd1958f29edd9d209e3c72bd1692'
import_commit='19147c74bbe6335331f3fdad256663906c122dc3'
import_parent='be08d82eb6d71681f78bd84bae6a37257d5c6d36'
workspace_commit='02039d879ee0d7ede772326b2fc816619f6ebf89'
cutover_commit='6cf468d277a88f085242bdaef017305e1148efda'
direct_probe_commit='8f937353bded0bae3e8429243a6b9d7c0e918229'
frontend_commit='95aaa4a4987dd63951410b19d8e693f29527eae6'
map='migration/loom-native.commit-map'
map_sha256='6f6bb23c88a4ff43791d5be109643b7527d48dc8baf0fb546583282884542ce0'
commit_count=77
merge_count=8

if command -v shasum >/dev/null 2>&1; then
  actual_map_sha256="$(shasum -a 256 "$map" | awk '{print $1}')"
else
  actual_map_sha256="$(sha256sum "$map" | awk '{print $1}')"
fi

test "$actual_map_sha256" = "$map_sha256"
test "$(wc -l < "$map" | tr -d ' ')" = "$((commit_count + 1))"
test "$(git rev-list --count "$filtered_head")" = "$commit_count"
test "$(git rev-list --merges --count "$filtered_head")" = "$merge_count"
test "$(git rev-parse "${filtered_head}:${prefix}")" = "$source_tree"
test "$(git show -s --format=%P "$import_commit")" = "$import_parent $filtered_head"
git merge-base --is-ancestor "$import_commit" HEAD
git merge-base --is-ancestor "$workspace_commit" HEAD
git merge-base --is-ancestor "$cutover_commit" HEAD
git merge-base --is-ancestor "$direct_probe_commit" HEAD
git merge-base --is-ancestor "$frontend_commit" HEAD

git fetch --no-tags "$source_repository" "$source_main"
test "$(git rev-parse FETCH_HEAD)" = "$source_main"
test "$(git rev-parse "${source_main}^{tree}")" = "$source_tree"
test "$(git rev-list --count "$source_main")" = "$commit_count"
test "$(git rev-list --merges --count "$source_main")" = "$merge_count"

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

echo 'Loom import history: 77 commits preserve topology, identity, dates, subjects, and byte-identical prefixed trees'
