#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

source_repository='https://github.com/delysis/llama-native-kit.git'
source_main='16168bd76a09f74fdee41d0e2fb0441e79ac1005'
source_tree='65f8d97c178188de8e44188b5a6adf0195cdc57f'
filtered_head='9e6d8c49887b8691a0836158f2c3ea68715e11e5'
import_commit='152a0dda9ba0d1096022d11ddbd08489f524ab31'
cutover_commit='c35c6b2d42f60939f3a3478212743c9c82f28b80'
map='migration/llama-native-kit.commit-map'
map_sha256='90089306976c5c43aabfb23781a7df563f9245724d46cd1c3043e2a817a4c897'

if command -v shasum >/dev/null 2>&1; then
  actual_map_sha256="$(shasum -a 256 "$map" | awk '{print $1}')"
else
  actual_map_sha256="$(sha256sum "$map" | awk '{print $1}')"
fi
test "$actual_map_sha256" = "$map_sha256"
test "$(wc -l < "$map" | tr -d ' ')" = 46
test "$(git rev-parse "${filtered_head}:crates/native")" = "$source_tree"
test "$(git show -s --format=%P "$import_commit")" = "68b4f87c331d9ea887713201d4ee479c3445226a $filtered_head"
git merge-base --is-ancestor "$import_commit" HEAD
git merge-base --is-ancestor "$cutover_commit" HEAD

git fetch --no-tags "$source_repository" "$source_main"
test "$(git rev-parse FETCH_HEAD)" = "$source_main"
test "$(git rev-parse "${source_main}^{tree}")" = "$source_tree"

test "$(awk 'NR > 1 { count++ } END { print count+0 }' "$map")" = 45
while read -r old new; do
  if [[ "$old" == old ]]; then
    continue
  fi
  test "$new" != 0000000000000000000000000000000000000000
  test "$(git rev-parse "${old}^{tree}")" = "$(git rev-parse "${new}:crates/native")"

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
done < "$map"

echo "native import history: 45 commits preserve topology and byte-identical prefixed trees"
