"""Widget publisher unit tests: translation invariants and honesty rules."""

from __future__ import annotations

import copy
import unittest

from openbb_gateway.widget_publisher import (
    WidgetPublishError,
    publishable_widgets,
    to_openbb_function_call,
    widget_uuid,
)


def artifact_fixture() -> dict:
    return {
        "format": "krw-visualization",
        "schema_version": 4,
        "title": "Southern Copper — quarterly revenue",
        "views": [
            {
                "view_id": "revenue_quarterly",
                "chart_type": "bar",
                "title": "Revenue by quarter",
                "unit": "USD millions",
                "series": [
                    {
                        "series_key": "SCCO/revenue",
                        "label": "SCCO revenue",
                        "points": [
                            {"period": "2025Q4", "value": 3200.0, "evidence_ref": "obj/1"},
                            {"period": "2026Q1", "value": 3350.0, "evidence_ref": "obj/2"},
                        ],
                    }
                ],
            },
            {
                "view_id": "pbr_trend",
                "chart_type": "line",
                "title": "PBR trend",
                "unit": "x",
                "series": [
                    {
                        "series_key": "SCCO/pbr",
                        "label": "PBR",
                        "points": [
                            {"period": "2025Q4", "value": 4.1, "evidence_ref": "obj/3"},
                        ],
                    }
                ],
            },
        ],
        "provenance": {
            "evidence_refs": ["obj/9"],
            "source_kinds": ["filing_metric"],
        },
    }


class PublishableWidgetsTests(unittest.TestCase):
    def test_one_widget_per_view_with_mapped_types(self) -> None:
        payloads = publishable_widgets(artifact_fixture(), origin="run-abc")
        self.assertEqual(len(payloads), 2)
        by_id = {payload["widget"]["widget_id"]: payload for payload in payloads}
        self.assertEqual(by_id["krw-revenue_quarterly"]["widget"]["widget_type"], "bar_chart")
        self.assertEqual(by_id["krw-pbr_trend"]["widget"]["widget_type"], "line_chart")

    def test_uuids_are_deterministic_and_origin_scoped(self) -> None:
        first = publishable_widgets(artifact_fixture(), origin="run-abc")
        again = publishable_widgets(artifact_fixture(), origin="run-abc")
        other = publishable_widgets(artifact_fixture(), origin="run-xyz")
        self.assertEqual(
            [p["widget"]["uuid"] for p in first],
            [p["widget"]["uuid"] for p in again],
        )
        self.assertNotEqual(
            first[0]["widget"]["uuid"], other[0]["widget"]["uuid"]
        )
        self.assertEqual(first[0]["widget"]["uuid"], widget_uuid("run-abc", "krw-revenue_quarterly"))

    def test_provenance_merges_view_and_artifact_evidence_refs(self) -> None:
        payloads = publishable_widgets(artifact_fixture(), origin="run-abc")
        refs = payloads[0]["data_sources"][0]["evidence_refs"]
        self.assertIn("obj/1", refs)
        self.assertIn("obj/2", refs)
        self.assertIn("obj/9", refs)  # artifact-level provenance survives

    def test_series_data_passes_through_untouched(self) -> None:
        fixture = artifact_fixture()
        payloads = publishable_widgets(fixture, origin="run-abc")
        self.assertEqual(
            payloads[0]["data_sources"][0]["series"],
            fixture["views"][0]["series"],
        )

    def test_unknown_chart_type_passes_through_verbatim(self) -> None:
        fixture = artifact_fixture()
        fixture["views"][0]["chart_type"] = "heatmap_fancy"
        payloads = publishable_widgets(fixture, origin="run-abc")
        self.assertEqual(payloads[0]["widget"]["widget_type"], "heatmap_fancy")

    def test_input_artifact_is_never_mutated(self) -> None:
        fixture = artifact_fixture()
        snapshot = copy.deepcopy(fixture)
        publishable_widgets(fixture, origin="run-abc")
        self.assertEqual(fixture, snapshot)

    def test_rejects_wrong_format_honestly(self) -> None:
        fixture = artifact_fixture()
        fixture["format"] = "something-else"
        with self.assertRaises(WidgetPublishError):
            publishable_widgets(fixture, origin="run-abc")

    def test_rejects_view_without_series(self) -> None:
        fixture = artifact_fixture()
        fixture["views"][1]["series"] = []
        with self.assertRaises(WidgetPublishError):
            publishable_widgets(fixture, origin="run-abc")

    def test_rejects_missing_origin(self) -> None:
        with self.assertRaises(WidgetPublishError):
            publishable_widgets(artifact_fixture(), origin="")


class FunctionCallEnvelopeTests(unittest.TestCase):
    def test_envelope_wraps_gateway_payload_without_sharing_state(self) -> None:
        payload = publishable_widgets(artifact_fixture(), origin="run-abc")[0]
        envelope = to_openbb_function_call(payload)
        self.assertEqual(envelope["name"], "create_widget")
        self.assertEqual(envelope["arguments"], payload)
        envelope["arguments"]["widget"]["title"] = "mutated"
        self.assertNotEqual(payload["widget"]["title"], "mutated")

    def test_envelope_rejects_foreign_documents(self) -> None:
        with self.assertRaises(WidgetPublishError):
            to_openbb_function_call({"format": "not-ours"})


if __name__ == "__main__":
    unittest.main()
