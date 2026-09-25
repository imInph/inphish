#!/usr/bin/env python3
# Fits the win/draw/loss model behind UCI_ShowWDL to inphish self-play PGNs from fastchess.
# Usage: tools/wdl_fit.py EVIDENCE_DIR
import glob, math, os, re, sys
files = [f for f in glob.glob(os.path.join(sys.argv[1], '*.pgn'))
         if re.search(r'vs-v0\.1\.0|aspiration|conthist|improving|newterms|qtt|tuned', f)]
samples = []
for path in files:
    text = open(path).read()
    for game in text.split('[Event ')[1:]:
        result = re.search(r'\[Result "([^"]+)"\]', game).group(1)
        if result not in ('1-0', '0-1', '1/2-1/2'):
            continue
        if re.search(r'\[Termination "(time forfeit|abandoned|illegal)', game):
            continue
        white = {'1-0': 1, '0-1': -1, '1/2-1/2': 0}[result]
        body = game.split('\n\n', 1)[1]
        comments = re.findall(r'\{([+-]?(?:M?\d+(?:\.\d+)?))/\d+', body)
        for ply, value in enumerate(comments):
            if ply < 16 or 'M' in value:
                continue
            cp = float(value) * 100
            if abs(cp) > 1200:
                continue
            outcome = white if ply % 2 == 0 else -white
            samples.append((cp, outcome))
print(len(files), 'files', len(samples), 'samples', file=sys.stderr)

def nll(a, b):
    total = 0.0
    for cp, outcome in samples:
        win = 1 / (1 + math.exp(-(cp - a) / b))
        loss = 1 / (1 + math.exp((cp + a) / b))
        p = win if outcome == 1 else loss if outcome == -1 else max(1e-9, 1 - win - loss)
        total -= math.log(max(p, 1e-9))
    return total / len(samples)

best = min(((nll(a, b), a, b) for a in range(0, 401, 20) for b in range(20, 401, 20)))
_, a0, b0 = best
best = min(((nll(a, b), a, b) for a in range(max(0, a0 - 20), a0 + 21, 2) for b in range(max(2, b0 - 20), b0 + 21, 2)))
print('nll %.4f a %d b %d' % best)
for cp in (0, 50, 100, 200, 400):
    _, a, b = best
    w = 1 / (1 + math.exp(-(cp - a) / b)); l = 1 / (1 + math.exp((cp + a) / b))
    print(cp, round(w * 1000), round((1 - w - l) * 1000), round(l * 1000))
