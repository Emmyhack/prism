import unittest

from prism import MoveClip, RationalTime, TransactionRequest


class ProtocolTests(unittest.TestCase):
    def test_emits_cli_compatible_exact_transaction(self) -> None:
        request = TransactionRequest(
            "python-test",
            4,
            "Move opening clip",
            (
                MoveClip(
                    "11111111-1111-4111-8111-111111111111",
                    "22222222-2222-4222-8222-222222222222",
                    "33333333-3333-4333-8333-333333333333",
                    RationalTime(1, 8),
                ),
            ),
        )
        payload = request.as_dict()
        self.assertEqual(payload["apiVersion"], "1")
        self.assertEqual(payload["operations"][0]["timelineStart"], {"value": 1, "timescale": 8})

    def test_rejects_zero_timescale(self) -> None:
        with self.assertRaises(ValueError):
            RationalTime(1, 0)


if __name__ == "__main__":
    unittest.main()
