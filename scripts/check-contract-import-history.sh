#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

source_repository='https://github.com/delysis/w1-platform-contracts.git'
source_main='3ed1f3235edb6d481c324f05fe83b2379e3431e6'
source_tree='55ad1fa9b7e3938043a153710922304022601a67'
prefix='crates/platform/contracts'
filtered_head='a2f04b051cf5a908699576d62c4a78c22d36e094'
import_commit='018aa483dbe34ecb3a62f70adc6bfebe99684acc'
import_parent='c7bb859a48b5274fc7ebfafa510c49563a76d9b3'
cutover_commit='1c79381f9111dfd2d266291db243c7a5091a7fe4'
map='migration/w1-platform-contracts.commit-map'
map_sha256='6f56105a268443356e0245b70a0638dbe43f1d3e9933360e4d62b4f986b54e3d'
commit_count=25

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

echo 'W1 contract import history: 25 commits preserve topology, identity, dates, subjects, and byte-identical prefixed trees'
