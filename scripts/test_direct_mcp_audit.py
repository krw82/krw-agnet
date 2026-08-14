"""Offline tests for the model-free MCP audit helpers."""

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from run_direct_mcp_audit import (
    AuditError,
    check_explicit_query_scope,
    check_ticker,
    dynamic_read_env_reference,
    env_file_value,
    parse_message,
    resolve_feed_token_from_env,
    resolve_feed_token_from_operator_env,
    structured_result,
    summarize_payload,
)


class DirectMcpAuditTests(unittest.TestCase):
    def test_sse_message_is_parsed_without_provider_calls(self) -> None:
        message = parse_message('event: message\ndata: {"jsonrpc":"2.0","result":{}}\n\n')
        self.assertEqual(message["jsonrpc"], "2.0")

    def test_tool_error_is_not_reported_as_empty_success(self) -> None:
        payload, is_error, shape_error = structured_result(
            {
                "jsonrpc": "2.0",
                "result": {"isError": True, "content": [{"type": "text", "text": "[]"}]},
            }
        )
        self.assertTrue(is_error)
        self.assertIsNone(shape_error)
        self.assertEqual(payload, {"items": []})

    def test_pagination_is_preserved_as_control_metadata(self) -> None:
        summary = summarize_payload(
            {"results": [{"id": "object:1"}], "pagination": {"count": 1, "has_more": True, "next_offset": 1}}
        )
        self.assertEqual(summary["results_count"], 1)
        self.assertEqual(summary["pagination"]["next_offset"], 1)
        self.assertTrue(summary["pagination"]["has_more"])

    def test_cross_ticker_row_is_rejected(self) -> None:
        with self.assertRaises(AuditError):
            check_ticker({"items": [{"ticker": "MSFT"}]}, "AAPL")

    def test_explicit_period_scope_rejects_a_stale_source_row(self) -> None:
        with self.assertRaises(AuditError):
            check_explicit_query_scope(
                {
                    "results": [
                        {"ticker": "AAPL", "document_type": "10-K", "period": "FY2021"}
                    ]
                },
                requested_period="FY2022",
                requested_document_type="10-K",
            )

    def test_explicit_annual_scope_accepts_fy_cy_alias(self) -> None:
        check_explicit_query_scope(
            {
                "results": [
                    {"ticker": "AAPL", "document_type": "10-K", "period": "CY2022"}
                ]
            },
            requested_period="FY2022",
            requested_document_type="10-K",
        )

    def test_metric_projection_period_does_not_override_source_filing_period(self) -> None:
        check_explicit_query_scope(
            {
                "results": [
                    {
                        "ticker": "AAPL",
                        "period": "FY2021",
                        "document": {"document_type": "10-K", "period": "CY2022"},
                        "object": {"document_type": "10-K", "period": "CY2022"},
                    }
                ]
            },
            requested_period="FY2022",
            requested_document_type="10-K",
        )

    def test_nested_stale_source_filing_is_rejected(self) -> None:
        with self.assertRaises(AuditError):
            check_explicit_query_scope(
                {
                    "results": [
                        {
                            "ticker": "AAPL",
                            "period": "CY2022",
                            "document": {"document_type": "10-K", "period": "CY2021"},
                        }
                    ]
                },
                requested_period="FY2022",
                requested_document_type="10-K",
            )

    def test_operator_env_export_prefix_is_read_without_exposing_value(self) -> None:
        with TemporaryDirectory() as directory:
            path = Path(directory) / "deploy.env"
            path.write_text("export KRW_FEED_MCP_TOKEN='opaque-value'\n", encoding="utf-8")
            self.assertEqual(env_file_value(path, "KRW_FEED_MCP_TOKEN"), "opaque-value")

    def test_front_env_alias_is_available_for_feed_audit(self) -> None:
        with TemporaryDirectory() as directory:
            path = Path(directory) / "front.env"
            path.write_text("FEED_MCP_AUTH_TOKEN=opaque-value\n", encoding="utf-8")
            self.assertEqual(env_file_value(path, "FEED_MCP_AUTH_TOKEN"), "opaque-value")

    def test_operator_dynamic_feed_reference_is_resolved_as_data_not_shell(self) -> None:
        with TemporaryDirectory() as directory:
            root = Path(directory)
            front = root / ".env"
            operator = root / "krw-agent-deploy.env"
            front.write_text("KRW_FEED_MCP_TOKEN=opaque-current-token\n", encoding="utf-8")
            operator.write_text(
                f'export KRW_FEED_MCP_TOKEN="$(read_env_value {front} KRW_FEED_MCP_TOKEN)"\n',
                encoding="utf-8",
            )

            resolution = resolve_feed_token_from_operator_env(operator)

            self.assertEqual(resolution.value, "opaque-current-token")
            self.assertEqual(resolution.source, "operator_dynamic_canonical")

    def test_dynamic_env_parser_rejects_anything_beyond_the_generated_form(self) -> None:
        self.assertIsNone(dynamic_read_env_reference("$(cat /tmp/token)"))
        self.assertIsNone(dynamic_read_env_reference("$(read_env_value relative.env KRW_FEED_MCP_TOKEN)"))

    def test_feed_alias_conflict_is_not_silently_prioritized(self) -> None:
        with TemporaryDirectory() as directory:
            path = Path(directory) / "front.env"
            path.write_text(
                "KRW_FEED_MCP_TOKEN=canonical\nFEED_MCP_AUTH_TOKEN=legacy\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(AuditError, "aliases conflict"):
                resolve_feed_token_from_env(path, "front_override")


if __name__ == "__main__":
    unittest.main()
