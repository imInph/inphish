#!/usr/bin/env bash
set -euo pipefail

if (( $# < 3 || $# > 4 )); then
    printf 'usage: %s REF OPPONENT_NAME OPPONENT_CMD [OPPONENT_ARGS]\n' "$0" >&2
    exit 2
fi

ref=$1
opponent_name=$2
opponent_cmd=$3
opponent_args=${4:-}
fastchess=${MATCH_FASTCHESS:-fastchess}
time_control=${MATCH_TC:-1+0.01}
rounds=${MATCH_ROUNDS:-10}
concurrency=${MATCH_CONCURRENCY:-2}
evidence=${MATCH_EVIDENCE:-$HOME/inphish-evidence}
read -r -a opponent_options <<< "${MATCH_OPPONENT_OPTIONS:-}"
# The development machine is a fanless laptop: a batch is capped at 40 games, 1+0.01,
# two concurrent games and 20 minutes of wall clock unless the owner approved more.
wall_clock_seconds=1200

repo_root=$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)
openings=${MATCH_OPENINGS:-$repo_root/tests/openings/balanced.epd}

if [[ $time_control != 1+0.01 || $rounds -gt 20 || $concurrency -gt 2 ]] \
    && [[ ${MATCH_OWNER_APPROVED:-} != yes ]]; then
    printf 'beyond the default budget; set MATCH_OWNER_APPROVED=yes only with owner approval\n' >&2
    exit 2
fi
if ! command -v "$fastchess" >/dev/null 2>&1; then
    printf 'fastchess not found: %s\n' "$fastchess" >&2
    exit 2
fi
if [[ ! -f $openings ]]; then
    printf 'opening file not found: %s\n' "$openings" >&2
    exit 2
fi

commit=$(git -C "$repo_root" rev-parse --verify "${ref}^{commit}")
short=$(git -C "$repo_root" rev-parse --short "$commit")
stamp=$(date +%Y%m%d-%H%M%S)
label="$stamp-$short-vs-$opponent_name-${time_control//+/_}"
mkdir -p "$evidence"

scratch=$(mktemp -d "${TMPDIR:-/tmp}/inphish-match.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
git -C "$repo_root" archive "$commit" | tar -xf - -C "$scratch"
cargo build --release --quiet --manifest-path "$scratch/Cargo.toml" --target-dir "$scratch/target"
engine="$evidence/$label-inphish"
cp "$scratch/target/release/inphish" "$engine"
printf 'bench: %s\n' "$("$engine" bench)" | tee "$evidence/$label.log"

opponent=(-engine "cmd=$opponent_cmd" "name=$opponent_name")
if [[ -n $opponent_args ]]; then
    opponent+=("args=$opponent_args")
fi
opponent+=(${opponent_options[@]+"${opponent_options[@]}"})

status=0
(
    cd "$scratch"
    perl -e 'alarm shift; exec @ARGV' "$wall_clock_seconds" \
        "$fastchess" \
        -engine "cmd=$engine" "name=inphish-$short" option.Hash=16 \
        "${opponent[@]}" \
        -each "tc=$time_control" \
        -openings "file=$openings" format=epd order=sequential \
        -rounds "$rounds" -games 2 -repeat -concurrency "$concurrency" \
        -recover -report penta=true \
        -pgnout "file=$evidence/$label.pgn"
) >> "$evidence/$label.log" 2>&1 || status=$?

if (( status == 142 )); then
    printf 'stopped at the %s-second wall-clock cap\n' "$wall_clock_seconds" | tee -a "$evidence/$label.log"
fi
grep -A5 '^Results of' "$evidence/$label.log" | tail -6 || true
printf 'terminations:\n'
grep -ho ', [^,}]*}' "$evidence/$label.pgn" | sed -E 's/^, //; s/}$//; s/ \([0-9]+ms overrun\)//' \
    | sort | uniq -c || true
printf 'log: %s\npgn: %s\n' "$evidence/$label.log" "$evidence/$label.pgn"
