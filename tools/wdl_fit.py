#!/usr/bin/env python3
# Fits UCI_ShowWDL to fastchess PGNs, holding out whole opening groups for validation.
import argparse
import hashlib
import json
import math
import re
from collections import defaultdict
from pathlib import Path


def collect(game, prefix, stride):
    tags = dict(re.findall(r'\[(\w+) "([^"\n]*)"\]', game))
    result = tags.get('Result')
    if result not in ('1-0', '0-1', '1/2-1/2') or tags.get('Termination') != 'normal':
        return None
    body = game.split('\n\n', 1)[-1]
    if '(' in body or ')' in body:
        return None
    fen = tags.get('FEN', 'startpos')
    first = 1 if fen == 'startpos' or fen.split()[1] == 'w' else -1
    names = (tags.get('White', ''), tags.get('Black', ''))
    if first == -1:
        names = names[::-1]
    white = {'1-0': 1, '0-1': -1, '1/2-1/2': 0}[result]
    samples = []
    ply = -1
    for token in re.findall(r'\{[^}]*\}|\$\d+|\d+\.(?:\.\.)?|[^\s{}]+', body):
        if token.startswith('{'):
            match = re.match(r'\{([+-]?\d+(?:\.\d+)?)/\d+', token)
            if not match or ply < 16 or ((ply - 16) // 2) % stride:
                continue
            if prefix and not names[ply % 2].startswith(prefix):
                continue
            cp = round(float(match[1]) * 100)
            if abs(cp) <= 1200:
                samples.append((cp, white * first * (-1 if ply % 2 else 1)))
        elif not token.startswith('$') and not re.fullmatch(r'\d+\.(?:\.\.)?', token):
            if token not in ('1-0', '0-1', '1/2-1/2', '*', '...'):
                ply += 1
    opening = ' '.join(fen.split()[:4])
    return (opening, samples) if samples else None


def histogram(games):
    counts = defaultdict(float)
    for _, samples in games:
        for cp, outcome in samples:
            counts[cp, outcome] += 1 / len(samples)
    return counts


def probabilities(cp, a, b):
    def logistic(x):
        z = math.exp(-abs(x))
        return z / (1 + z) if x >= 0 else 1 / (1 + z)
    win = logistic((a - cp) / b)
    loss = logistic((a + cp) / b)
    return win, max(0, 1 - win - loss), loss


def nll(counts, a, b):
    total = 0.0
    for (cp, outcome), weight in counts.items():
        win, draw, loss = probabilities(cp, a, b)
        probability = win if outcome == 1 else loss if outcome == -1 else draw
        total -= weight * math.log(max(1e-12, probability))
    return total / sum(counts.values())


def fit(counts):
    best = min((nll(counts, a, b), a, b)
               for a in range(20, 1001, 20) for b in range(20, 601, 20))
    _, a, b = best
    return min((nll(counts, x, y), x, y)
               for x in range(max(2, a - 20), a + 21, 2)
               for y in range(max(2, b - 20), b + 21, 2))


def main():
    parser = argparse.ArgumentParser(description=__doc__ or 'Fit WDL to fastchess evaluations.')
    parser.add_argument('evidence', type=Path)
    parser.add_argument('pattern', nargs='?', default=r'0a12cfe-vs-(inphish-6.2.0|SF19-full|MaiV3-h16)')
    parser.add_argument('prefix', nargs='?', default='inphish-0a12cfe')
    parser.add_argument('--stride', type=int, default=4, help='sample every N moves per side')
    parser.add_argument('--baseline', nargs=2, type=float, default=(714, 290), metavar=('A', 'B'))
    parser.add_argument('--json', type=Path, help='save the fit and validation report')
    args = parser.parse_args()
    if args.stride < 1 or args.baseline[0] <= 0 or args.baseline[1] <= 0:
        parser.error('stride and baseline parameters must be positive')
    files = sorted(path for path in args.evidence.glob('*.pgn') if re.search(args.pattern, path.name))
    games = []
    for path in files:
        for game in path.read_text().split('[Event ')[1:]:
            data = collect(game, args.prefix, args.stride)
            if data:
                games.append(data)
    training, validation = [], []
    for game in games:
        group = int.from_bytes(hashlib.sha256(game[0].encode()).digest()[:4], 'big')
        (validation if group % 5 == 0 else training).append(game)
    if not training or not validation:
        parser.error('need games in both training and held-out opening groups')
    trained = fit(histogram(training))
    held_out = histogram(validation)
    final = fit(histogram(games))
    report = {
        'files': [path.name for path in files],
        'prefix': args.prefix,
        'stride': args.stride,
        'games': len(games),
        'samples': sum(len(samples) for _, samples in games),
        'training_games': len(training),
        'validation_games': len(validation),
        'training_fit': {'a': trained[1], 'b': trained[2], 'nll': trained[0]},
        'validation_baseline_nll': nll(held_out, *args.baseline),
        'validation_fitted_nll': nll(held_out, *trained[1:]),
        'final_fit': {'a': final[1], 'b': final[2], 'nll': final[0]},
    }
    print(json.dumps(report, indent=2))
    for cp in (0, 50, 100, 200, 400):
        win, _, loss = probabilities(cp, *final[1:])
        w, l = round(win * 1000), round(loss * 1000)
        print(cp, w, 1000 - w - l, l)
    if args.json:
        args.json.write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
