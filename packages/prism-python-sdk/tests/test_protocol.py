import unittest

from prism import (
    AddMarker, AddTransition, CreateSequence, CreateTrack, DeleteClip, DeleteMarker, DeleteTransition, DuplicateClip, ImportMedia, InsertClip, MoveClip,
    RationalTime, RecipeRequest, ReorderTrack, RippleDelete, RollEdit, SetTrackLocked, SetTrackMuted, SetTrackVisible, SlipClip, SplitClip, TransactionRequest, TrimClip,
)


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

    def test_rejects_invalid_transaction_envelope_fields(self) -> None:
        with self.assertRaises(ValueError):
            TransactionRequest("x", -1, "label", ()).as_dict()
        with self.assertRaises(TypeError):
            TransactionRequest("x", 0, "label", []).as_dict()
        with self.assertRaises(TypeError):
            TransactionRequest("x", 0, "label", (), dry_run="yes").as_dict()

    def test_emits_shared_recipe_request(self) -> None:
        request = RecipeRequest(
            {"id": "prism.example.recipe", "apiVersion": 1},
            {"clipCount": 5},
        )
        self.assertEqual(
            request.as_dict(),
            {
                "manifest": {"id": "prism.example.recipe", "apiVersion": 1},
                "inputs": {"clipCount": 5},
            },
        )
        self.assertTrue(request.to_json().endswith("\n"))

    def test_emits_remaining_core_clip_operations(self) -> None:
        time = RationalTime(1, 8)
        self.assertEqual(
            TrimClip("s", "t", "c", time, time, time).as_dict()["type"], "trimClip"
        )
        self.assertEqual(
            SplitClip("s", "t", "c", "r", time).as_dict()["type"], "splitClip"
        )
        self.assertEqual(DeleteClip("s", "t", "c").as_dict()["type"], "deleteClip")
        self.assertEqual(
            SlipClip("s", "t", "c", time).as_dict()["sourceDelta"],
            {"value": 1, "timescale": 8},
        )
        self.assertEqual(
            RollEdit("s", "t", "a", "b", time).as_dict()["delta"],
            {"value": 1, "timescale": 8},
        )

    def test_emits_project_building_operations(self) -> None:
        time = RationalTime(0, 1)
        self.assertEqual(ImportMedia("m", "source.mp4").as_dict()["type"], "importMedia")
        self.assertEqual(CreateSequence("s", "Main").as_dict()["type"], "createSequence")
        self.assertEqual(CreateTrack("s", "t", "video").as_dict()["kind"], "video")
        self.assertEqual(
            InsertClip("s", "t", "c", "m", time, time, RationalTime(1, 1)).as_dict()["type"],
            "insertClip",
        )

    def test_emits_track_lock_operation(self) -> None:
        self.assertEqual(
            SetTrackLocked("s", "t", True).as_dict(),
            {"type": "setTrackLocked", "sequenceId": "s", "trackId": "t", "locked": True},
        )

    def test_emits_track_reorder_operation(self) -> None:
        self.assertEqual(
            ReorderTrack("s", "t", 2).as_dict(),
            {"type": "reorderTrack", "sequenceId": "s", "trackId": "t", "destinationIndex": 2},
        )

    def test_emits_track_visibility_and_mute_operations(self) -> None:
        self.assertEqual(SetTrackVisible("s", "t", False).as_dict()["type"], "setTrackVisible")
        self.assertEqual(SetTrackMuted("s", "t", True).as_dict()["type"], "setTrackMuted")

    def test_emits_marker_operations(self) -> None:
        self.assertEqual(
            AddMarker("s", "m", RationalTime(3, 2), "Beat").as_dict(),
            {"type": "addMarker", "sequenceId": "s", "id": "m", "time": {"value": 3, "timescale": 2}, "label": "Beat"},
        )
        self.assertEqual(DeleteMarker("s", "m").as_dict()["type"], "deleteMarker")

    def test_emits_duplicate_clip_operation(self) -> None:
        self.assertEqual(DuplicateClip("s", "t", "c", "d", RationalTime(4, 1)).as_dict()["type"], "duplicateClip")

    def test_emits_ripple_delete_operation(self) -> None:
        self.assertEqual(RippleDelete("s", "t", "c").as_dict()["type"], "rippleDelete")

    def test_emits_transition_operations(self) -> None:
        self.assertEqual(AddTransition("s", "x", "a", "b", RationalTime(1, 2)).as_dict()["type"], "addTransition")
        self.assertEqual(DeleteTransition("s", "x").as_dict()["type"], "deleteTransition")


if __name__ == "__main__":
    unittest.main()
