#!/usr/bin/env python3
"""Small offline tests for provider-bound live acceptance receipts."""

from __future__ import annotations

import unittest

from collect_release_evidence import validate_live_receipt


HASH = "sha256:" + "a" * 64


def receipt(provider: str, model: str) -> dict[str, object]:
    return {
        "schema_version": "krw-live-acceptance/v1",
        "status": "pass",
        "redacted": True,
        "provider_id": provider,
        "requested_model": model,
        "observed_model": model,
        "release_set_hash": HASH,
        "data_release_hash": HASH,
        "provider_contract_hash": HASH,
        "mcp_contract_hash": HASH,
        "postgres_fault_matrix_hash": HASH,
        "quality_report_hash": HASH,
    }


class ProviderBoundReceiptTest(unittest.TestCase):
    def test_glm_receipt_is_bound_to_glm(self) -> None:
        validate_live_receipt(receipt("glm", "glm-5.2"), HASH, "glm")

    def test_deepseek_receipt_is_bound_to_deepseek(self) -> None:
        validate_live_receipt(receipt("deepseek", "deepseek-v4-flash"), HASH, "deepseek")

    def test_mixed_provider_receipt_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            validate_live_receipt(receipt("glm", "glm-5.2"), HASH, "deepseek")


if __name__ == "__main__":
    unittest.main()
