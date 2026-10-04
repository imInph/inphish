import unittest

from wdl_fit import collect, histogram, probabilities


class WdlFitTests(unittest.TestCase):
    def test_missing_annotations_and_black_to_move_keep_the_right_side(self):
        game = '''[White "opponent"]
[Black "candidate"]
[Result "0-1"]
[Termination "normal"]
[FEN "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1"]

1... e5 {book} 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7
6. Re1 b5 7. Bb3 d6 8. c3 O-O 9. h3 Nb8 {+0.50/12 0.01s} 0-1
'''
        opening, samples = collect(game, 'candidate', 1)
        self.assertTrue(opening.endswith('b KQkq -'))
        self.assertEqual(samples, [(50, 1)])
        self.assertIsNone(collect(game.replace('normal', 'time forfeit'), 'candidate', 1))

    def test_each_game_has_equal_weight(self):
        counts = histogram([('opening', [(10, 1)]), ('other', [(20, 0)] * 10)])
        self.assertAlmostEqual(counts[10, 1], counts[20, 0])

    def test_probabilities_remain_finite_at_extreme_scores(self):
        for cp in (-1200, 0, 1200):
            win, draw, loss = probabilities(cp, 1000, 2)
            self.assertAlmostEqual(win + draw + loss, 1)
            self.assertTrue(all(0 <= value <= 1 for value in (win, draw, loss)))
            self.assertEqual((win, draw, loss), probabilities(-cp, 1000, 2)[::-1])


if __name__ == '__main__':
    unittest.main()
