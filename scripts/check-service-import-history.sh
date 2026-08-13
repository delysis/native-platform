#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

digest() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

verify_service() {
  local name="$1"
  local source_repository="$2"
  local source_main="$3"
  local source_tree="$4"
  local prefix="$5"
  local filtered_head="$6"
  local import_commit="$7"
  local import_parent="$8"
  local map="$9"
  local map_sha256="${10}"
  local commit_count="${11}"

  test "$(digest "$map")" = "$map_sha256"
  test "$(wc -l < "$map" | tr -d ' ')" = "$((commit_count + 1))"
  test "$(git rev-list --count "$filtered_head")" = "$commit_count"
  test "$(git rev-parse "${filtered_head}:${prefix}")" = "$source_tree"
  test "$(git show -s --format=%P "$import_commit")" = "$import_parent $filtered_head"
  git merge-base --is-ancestor "$import_commit" HEAD

  git fetch --no-tags "$source_repository" "$source_main"
  test "$(git rev-parse FETCH_HEAD)" = "$source_main"
  test "$(git rev-parse "${source_main}^{tree}")" = "$source_tree"
  test "$(git rev-list --count "$source_main")" = "$commit_count"

  local mapped_count=0
  while read -r old new; do
    if [[ "$old" == old ]]; then
      continue
    fi
    test "$new" != 0000000000000000000000000000000000000000
    test "$(git rev-parse "${old}^{tree}")" = "$(git rev-parse "${new}:${prefix}")"

    local expected_parents=''
    local old_parent mapped_parent
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
    mapped_count=$((mapped_count + 1))
  done < "$map"
  test "$mapped_count" = "$commit_count"

  printf '%s import history: %s commits preserve topology and byte-identical prefixed trees\n' \
    "$name" "$commit_count"
}

verify_service \
  attachment \
  https://github.com/delysis/attachment-native-kit.git \
  2a8d3a9a1828162a51185d207822ceb1ba6283a8 \
  b3863274df0535fe445c8295d7a6866ddcba1634 \
  crates/services/attachment \
  0051568608b41a71d43e66899ca2a34345a7f74e \
  5e82ed646bad0f57480f809cedf0cc2745b39dc6 \
  6ecbaec3f42adb7dbe63199c0a9217f367548241 \
  migration/attachment-native-kit.commit-map \
  931fdce49db3ce68e278570f782f20f42d866bffbae55685ef79c5500d92b495 \
  7

verify_service \
  information \
  https://github.com/delysis/information-native-kit.git \
  7cb255a6f8dda1db7d8e7242f3aa256be06e1bfe \
  519aad6ce9dd51f52debbb3b2061a7e2810c3d24 \
  crates/services/information \
  8d24a77e28c9a5d82d58f7206d664d419c10e577 \
  b73feb2649c2096505f6489023acf325117c267c \
  5e82ed646bad0f57480f809cedf0cc2745b39dc6 \
  migration/information-native-kit.commit-map \
  aadf2bed72b68065cf9d6442697649b6762e7306a9483e1cf09f9301667c15a9 \
  21

verify_service \
  speech \
  https://github.com/delysis/speech-native-kit.git \
  b836318f10a7e11f433ec3ea8dfa48707adc9b06 \
  ae104be383a3ab5bbcb6a5e7c4d4f83cb9cad706 \
  crates/services/speech \
  cb1c75a2a15d903712992efd0dad81e6def1f1d3 \
  4a45947508cf33fb0f8043e0507f2dda86d5d75c \
  b73feb2649c2096505f6489023acf325117c267c \
  migration/speech-native-kit.commit-map \
  b0d954d0c76ed4e7a05b04eb355bfa0e10f8dd7979c625541e4dfd7621ad7a92 \
  25

echo 'service import history: 53 commits preserved across three separate histories'
