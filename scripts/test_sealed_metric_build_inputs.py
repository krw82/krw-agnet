#!/usr/bin/env python3
"""Regression checks for the repository-sealed metric build input.

The metric dictionary is part of the agent release.  Rust builds must never
silently change because a sibling checkout or an ambient environment variable
is present, and a missing bundled dictionary must stop the build rather than
emit an empty capability contract.
"""

from __future__ import annotations

import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
BUILD_SCRIPTS = (
    ROOT / "crates/krw-contracts/build.rs",
    ROOT / "crates/research-planner/build.rs",
)
DICTIONARY = (
    ROOT
    / "services/krw-ontology-runtime/src/krw_capability_runtime/resources"
    / "ontology/schema/metric_dictionary.yaml"
)


class SealedMetricBuildInputTest(unittest.TestCase):
    def test_bundled_dictionary_is_present_and_nonempty(self) -> None:
        text = DICTIONARY.read_text(encoding="utf-8")
        self.assertIn("canonical_metrics:", text)
        metric_lines = [line for line in text.splitlines() if line.startswith("  ")]
        self.assertGreater(len(metric_lines), 0)

    def test_builds_do_not_read_an_ambient_ontology_checkout(self) -> None:
        for path in BUILD_SCRIPTS:
            with self.subTest(path=path):
                source = path.read_text(encoding="utf-8")
                self.assertNotIn("KRW_ONTOLOGY_ROOT", source)
                self.assertNotIn("rerun-if-env-changed", source)

    def test_missing_dictionary_cannot_generate_an_empty_contract(self) -> None:
        forbidden = (
            "generate_empty_module",
            "emitting empty metric list",
            "No metric dictionary was available",
            "Option<PathBuf>",
        )
        for path in BUILD_SCRIPTS:
            with self.subTest(path=path):
                source = path.read_text(encoding="utf-8")
                for marker in forbidden:
                    self.assertNotIn(marker, source)


if __name__ == "__main__":
    unittest.main()
