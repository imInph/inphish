#!/usr/bin/env python3
# Random 8-ply openings from the start position, kept when Stockfish 15.1 at depth 12
# scores them within 50 cp. Usage: tools/random_openings.py STOCKFISH COUNT SEED > book.epd
# tests/openings/random8.epd came from Stockfish 15.1 with COUNT 300 and SEED 20260926.
import random, subprocess, sys
sf = subprocess.Popen([sys.argv[1]], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
def send(line): sf.stdin.write(line + '\n')
def lines_until(prefix):
    out = []
    while True:
        line = sf.stdout.readline().strip()
        out.append(line)
        if line.startswith(prefix): return out
send('uci'); lines_until('uciok'); send('setoption name Hash value 16')
random.seed(int(sys.argv[3]))
count = int(sys.argv[2]); seen = set(); made = 0
while made < count:
    moves = []
    for _ in range(8):
        send('position startpos moves ' + ' '.join(moves)); send('go perft 1')
        legal = [l.split(':')[0] for l in lines_until('Nodes searched') if ':' in l and not l.startswith('Nodes')]
        if not legal: break
        moves.append(random.choice(legal))
    if len(moves) < 8: continue
    send('position startpos moves ' + ' '.join(moves)); send('d')
    fen = [l for l in lines_until('Checkers') if l.startswith('Fen:')][0][5:]
    key = ' '.join(fen.split()[:4])
    if key in seen: continue
    send('go depth 12')
    info = [l for l in lines_until('bestmove') if ' score ' in l][-1].split()
    kind, value = info[info.index('score') + 1], int(info[info.index('score') + 2])
    if kind == 'cp' and abs(value) <= 50:
        seen.add(key); made += 1
        print(key, flush=True)
send('quit')
