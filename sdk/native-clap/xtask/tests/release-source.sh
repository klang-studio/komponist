#!/usr/bin/env bash
# Exercise the release source guard in a small Git checkout without compiling the SDK.
set -euo pipefail

sdk="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
repo="$scratch/checkout"
sdk_dir="$repo/sdk/native-clap"
mkdir -p "$sdk_dir/xtask" "$sdk_dir/vendor/nice-plug" "$scratch/bin"
cp "$sdk/../../.gitignore" "$repo/.gitignore"
cp "$sdk/xtask/check-bundle.sh" "$sdk_dir/xtask/check-bundle.sh"
printf 'Fixture license\n' > "$repo/LICENSE"
printf 'Fixture ISC license\n' > "$sdk_dir/vendor/nice-plug/LICENSE"
printf 'Tracked source\n' > "$repo/source"
git -C "$repo" init --quiet --template=
git -C "$repo" add .
git -C "$repo" -c user.name='Release test' -c user.email=release-test@example.invalid \
    commit --quiet --no-gpg-sign --no-verify -m 'Release guard fixture'

export RELEASE_TEST_REVISION="$(git -C "$repo" rev-parse HEAD)"
export RELEASE_TEST_TARGET=x86_64-unknown-linux-gnu
export RELEASE_TEST_REPO="$repo"
export RELEASE_TEST_TRACE="$scratch/cargo-calls"
export PATH="$scratch/bin:$PATH"
unset CARGO_TARGET_DIR

cat > "$scratch/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2 $3 $4 $5" == 'run --locked -p native-clap-xtask --' ]]
shift 5
command="$1"
shift
printf '%s\n' "$command" >> "$RELEASE_TEST_TRACE"
case "$command" in
    bundle)
        while [[ "$1" != --out-dir ]]; do shift; done
        mkdir -p "$2"
        printf 'CLAP binary\n' > "$2/native_clap_example.clap"
        ;;
    validate)
        if [[ "$1" == */extracted/* && "${RELEASE_TEST_DIRTY_DURING_BUILD:-0}" == 1 ]]; then
            printf 'Concurrent edit\n' >> "$RELEASE_TEST_REPO/source"
        fi
        printf '{"plugin":{},"params":[]}\n'
        ;;
    metadata)
        [[ "$1" == */extracted/native_clap_example.clap ]]
        [[ -f "$2" && "$3" == "$RELEASE_TEST_TARGET" && "$4" == "$RELEASE_TEST_REVISION" ]]
        [[ "$5" == "target/native-clap-dist/native-clap-example-$3.json" ]]
        printf '{}\n' > "$5"
        ;;
    *) exit 2 ;;
esac
MOCK
chmod +x "$scratch/bin/cargo"

cd "$sdk_dir"
expect_rejected() {
    local message="$1" revision="$2"
    : > "$RELEASE_TEST_TRACE"
    if bash xtask/check-bundle.sh "$RELEASE_TEST_TARGET" "$revision" > "$scratch/diagnostic" 2>&1; then
        echo "Release unexpectedly succeeded" >&2
        exit 1
    fi
    grep -Fq "$message" "$scratch/diagnostic"
}

mkdir -p target
printf 'Ignored build output\n' > target/ignored
bash xtask/check-bundle.sh "$RELEASE_TEST_TARGET" "$RELEASE_TEST_REVISION" > "$scratch/clean"
grep -Fxq metadata "$RELEASE_TEST_TRACE"
test -f "target/native-clap-dist/native-clap-example-$RELEASE_TEST_TARGET.json"

expect_rejected 'differs from checkout HEAD' 0000000000000000000000000000000000000000
test ! -s "$RELEASE_TEST_TRACE"

printf 'Unstaged change\n' >> "$repo/source"
expect_rejected 'requires a clean checkout' "$RELEASE_TEST_REVISION"
test ! -s "$RELEASE_TEST_TRACE"

git -C "$repo" add source
expect_rejected 'requires a clean checkout' "$RELEASE_TEST_REVISION"
test ! -s "$RELEASE_TEST_TRACE"
git -C "$repo" restore --source=HEAD --staged --worktree source

touch untracked-source
expect_rejected 'requires a clean checkout' "$RELEASE_TEST_REVISION"
test ! -s "$RELEASE_TEST_TRACE"
rm untracked-source

RELEASE_TEST_DIRTY_DURING_BUILD=1 expect_rejected 'requires a clean checkout' "$RELEASE_TEST_REVISION"
grep -Fxq validate "$RELEASE_TEST_TRACE"
if grep -Fxq metadata "$RELEASE_TEST_TRACE"; then
    echo "Metadata was generated after the source changed" >&2
    exit 1
fi
