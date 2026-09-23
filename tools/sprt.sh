#!/usr/bin/env bash
set -euo pipefail

if (( $# != 4 )); then
    printf 'usage: %s CANDIDATE_REF BASELINE_REF OPENINGS_EPD OUTPUT_PGN\n' "$0" >&2
    exit 2
fi

candidate_ref=$1
baseline_ref=$2
openings=$3
output=$4
fastchess=${SPRT_FASTCHESS:-fastchess}
time_control=${SPRT_TC:-8+0.08}
rounds=${SPRT_ROUNDS:-100000}
concurrency=${SPRT_CONCURRENCY:-4}
elo0=${SPRT_ELO0:-0}
elo1=${SPRT_ELO1:-5}

if [[ ! -f $openings ]]; then
    printf 'opening file not found: %s\n' "$openings" >&2
    exit 2
fi
if ! command -v "$fastchess" >/dev/null 2>&1; then
    printf 'fastchess not found: %s\n' "$fastchess" >&2
    exit 2
fi

repo_root=$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)
candidate_commit=$(git -C "$repo_root" rev-parse --verify "${candidate_ref}^{commit}")
baseline_commit=$(git -C "$repo_root" rev-parse --verify "${baseline_ref}^{commit}")
openings=$(cd "$(dirname "$openings")" && printf '%s/%s' "$PWD" "$(basename "$openings")")
output=$(cd "$(dirname "$output")" && printf '%s/%s' "$PWD" "$(basename "$output")")

scratch=$(mktemp -d "${TMPDIR:-/tmp}/inphish-sprt.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
mkdir -p "$scratch/candidate" "$scratch/baseline"

git -C "$repo_root" archive "$candidate_commit" | tar -xf - -C "$scratch/candidate"
git -C "$repo_root" archive "$baseline_commit" | tar -xf - -C "$scratch/baseline"

cargo build --release --manifest-path "$scratch/candidate/Cargo.toml" --target-dir "$scratch/candidate/target"
cargo build --release --manifest-path "$scratch/baseline/Cargo.toml" --target-dir "$scratch/baseline/target"

(
    cd "$scratch"
    "$fastchess" \
        -engine "cmd=$scratch/candidate/target/release/inphish" name=candidate \
        -engine "cmd=$scratch/baseline/target/release/inphish" name=baseline \
        -openings "file=$openings" format=epd order=random \
        -srand 1 \
        -each "tc=$time_control" \
        -sprt "elo0=$elo0" "elo1=$elo1" alpha=0.05 beta=0.05 \
        -rounds "$rounds" -repeat -concurrency "$concurrency" \
        -pgnout "file=$output"
)
