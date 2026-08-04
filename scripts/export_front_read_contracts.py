#!/usr/bin/env python3
"""Export the audited read-only krw-ontology-front MCP ABI snapshot.

The TypeScript/Zod implementation remains the semantic authority.  This
exporter deliberately does not try to infer return types from prompts or from
runtime samples: the closed schemas below mirror the cited source declarations
and serializers, while SOURCE_PINS makes source drift a hard export failure.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import tempfile
from pathlib import Path
from typing import Any


FORMAT = "krw-front-contract-export/v1"
DIALECT = "https://json-schema.org/draft/2020-12/schema"
SOURCE_PINS = {
    "src/krw-feed-mcp/server.ts": "87e130a88ee747c7caf84a52d3d1069e58ba609631eaaf0723ac8e496d58fd23",
    "src/krw-feed-mcp/feed-reader.ts": "b8be1046707c87e99eb1f0c13dd20dc5380a6641a44c54fcd7a03d1e78898896",
    "src/filings-mcp/server.ts": "585c775d25f2f65679d51d04cd6ed4c798df2c7a597d76587ff9c081648bfe14",
    "src/filings-mcp/on-demand-filing-reader.ts": "ff37bc416aa19bc4152c5bf64bdddf3cf4d830d7f17e6534a2a4e5a928313791",
    "src/types/database.ts": "a461b0d2937fbd53569e00a31f13b087aa29660b0f68ea92a4b495ecd75fa011",
    "src/lib/filing-briefs/contract.ts": "b991653e6b819c4ea7b786873fa8a9f805ffc3f1641ffd65cca03ae035f17dfe",
    "src/lib/filing-notifications/sec-filing-sections.ts": "b5f9c98612b4b6623bc610d4851e8427aa4078d48bc7652871c1df7e56885866",
    "src/lib/filing-notifications/sec-filing-index.ts": "9165909ccd7125156a23f6fba9089a237ee2f9f2687be880bf8b981240848c7e",
}


def canonical(value: Any) -> bytes:
    # This artifact set contains only integer schema literals, so Python's
    # canonical separators/key order are byte-identical to RFC 8785 here.
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def sha(data: bytes) -> str:
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def string(*, minimum: int = 0, maximum: int, pattern: str | None = None,
           format_: str | None = None, enum: list[str] | None = None,
           const: str | None = None) -> dict[str, Any]:
    result: dict[str, Any] = {"type": "string", "maxLength": maximum}
    if minimum:
        result["minLength"] = minimum
    if pattern:
        result["pattern"] = pattern
    if format_:
        result["format"] = format_
    if enum:
        result["enum"] = enum
    if const:
        result["const"] = const
    return result


def integer(minimum: int, maximum: int) -> dict[str, Any]:
    return {"type": "integer", "minimum": minimum, "maximum": maximum}


def number(minimum: int | float, maximum: int | float) -> dict[str, Any]:
    return {"type": "number", "minimum": minimum, "maximum": maximum}


def array(items: dict[str, Any], maximum: int, minimum: int = 0,
          unique: bool = False) -> dict[str, Any]:
    result: dict[str, Any] = {"type": "array", "items": items, "maxItems": maximum}
    if minimum:
        result["minItems"] = minimum
    if unique:
        result["uniqueItems"] = True
    return result


def obj(properties: dict[str, Any], required: list[str] | None = None) -> dict[str, Any]:
    return {
        "type": "object",
        "properties": properties,
        "required": required if required is not None else list(properties),
        "additionalProperties": False,
    }


def nullable(schema: dict[str, Any]) -> dict[str, Any]:
    return {"anyOf": [schema, {"type": "null"}]}


def ref(name: str) -> dict[str, str]:
    return {"$ref": f"#/$defs/{name}"}


UUID = string(maximum=36, minimum=36, pattern=r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-5][0-9a-fA-F]{3}-[89aAbB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$")
TICKER16 = string(maximum=16, minimum=1, pattern=r"^[A-Z0-9.-]{1,16}$")
TICKER20 = string(maximum=20, minimum=1, pattern=r"^[A-Z0-9.-]{1,20}$")
TICKER16_INPUT = string(maximum=16, minimum=1)
TICKER20_INPUT = string(maximum=20, minimum=1)
DATETIME = string(maximum=64, minimum=1, format_="date-time")
DATE = string(maximum=10, minimum=10, format_="date")
HTTPS_URL = string(maximum=2_000, minimum=8, format_="uri", pattern=r"^https://")
SHA256 = string(maximum=71, minimum=71, pattern=r"^sha256:[0-9a-f]{64}$")
ENVIRONMENT = string(maximum=7, enum=["dev", "staging", "prod"])
FORM_TYPE = string(maximum=5, enum=["8-K", "8-K/A", "6-K", "6-K/A", "4", "4/A"])
MAX_NUMBER = 1_000_000_000_000_000_000


JSON_VALUE: dict[str, Any] = {
    "anyOf": [
        {"type": "null"},
        {"type": "boolean"},
        {"type": "number", "minimum": -MAX_NUMBER, "maximum": MAX_NUMBER},
        string(maximum=8_192),
        {"type": "array", "items": ref("json_value"), "maxItems": 128},
        {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 128},
    ]
}


def feed_defs(max_source_posts: int) -> dict[str, Any]:
    entity = obj({
        "ticker": nullable(TICKER16),
        "name": string(maximum=120),
        "relationship": string(maximum=80),
        "relationship_reason": string(maximum=240),
        "confidence": number(0, 1),
    })
    post = obj({
        "id": string(maximum=128),
        "source_type": string(maximum=64),
        "external_item_id": string(maximum=256),
        "x_post_id": string(maximum=128),
        "filing_event_id": nullable(UUID),
        "source_class": string(maximum=64),
        "publisher": string(maximum=120),
        "author_handle": nullable(string(maximum=120, minimum=1)),
        "canonical_url": HTTPS_URL,
        "original_text": string(maximum=1_200),
        "published_at": DATETIME,
        "public_metrics": {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 64},
        "source_metadata": {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 64},
    })
    issue = obj({
        "id": UUID,
        "event_key": string(maximum=256),
        "title_ko": string(maximum=240),
        "summary_ko": string(maximum=1_500),
        "impact_path": array(string(maximum=360, minimum=1), 5),
        "confirmed_facts": array(string(maximum=360, minimum=1), 5),
        "unconfirmed_facts": array(string(maximum=360, minimum=1), 5),
        "status_labels": array(string(maximum=80, minimum=1), 3),
        "source_summary": {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 64},
        "source_count": integer(0, 1_000_000),
        "market_score": number(-1_000_000_000, 1_000_000_000),
        "risk_level": string(maximum=32),
        "first_observed_at": nullable(DATETIME),
        "published_at": nullable(DATETIME),
        "updated_at": nullable(DATETIME),
        "entities": array(ref("feed_entity"), 64),
        "source_posts": array(ref("feed_post"), max_source_posts),
    })
    media = obj({
        "id": string(maximum=128),
        "ordinal": integer(0, 1_000),
        "media_type": string(maximum=64),
        "media_url": nullable(HTTPS_URL),
        "content_hash": nullable(string(maximum=256, minimum=1)),
        "extraction_status": string(maximum=64),
        "extraction_model": nullable(string(maximum=160, minimum=1)),
        "extraction_confidence": nullable(number(0, 1)),
        "extraction": {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 128},
    })
    material = obj({
        "issue_id": UUID,
        "source_item_id": string(maximum=128),
        "source_type": string(maximum=64),
        "external_item_id": string(maximum=256),
        "x_post_id": string(maximum=128),
        "filing_event_id": nullable(UUID),
        "source_class": string(maximum=64),
        "publisher": string(maximum=120),
        "author_handle": nullable(string(maximum=120, minimum=1)),
        "canonical_url": HTTPS_URL,
        "published_at": DATETIME,
        "title": nullable(string(maximum=1_000, minimum=1)),
        "content_kind": nullable(string(maximum=80, minimum=1)),
        "signal_level": nullable(string(maximum=80, minimum=1)),
        "extraction": obj({
            "method": nullable(string(maximum=160, minimum=1)),
            "confidence": nullable(number(0, 1)),
        }),
        "source_metadata": {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 64},
        "original_text": string(maximum=25_000),
        "original_text_truncated": {"type": "boolean"},
        "original_text_omitted_due_to_budget": {"type": "boolean"},
        "original_text_hash": nullable(string(maximum=256, minimum=1)),
        "public_metrics": {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 64},
        "media": array(ref("feed_media"), 32),
    })
    packet = obj({
        "issue_id": UUID,
        "id": string(maximum=128),
        "version": integer(0, 1_000_000),
        "status": string(maximum=64),
        "source_context_hash": string(maximum=256),
        "generated_at": nullable(DATETIME),
        "packet": {"type": "object", "additionalProperties": ref("json_value"), "maxProperties": 128},
    })
    return {
        "json_value": JSON_VALUE,
        "feed_entity": entity,
        "feed_post": post,
        "feed_issue": issue,
        "feed_media": media,
        "feed_source_material": material,
        "feed_research_packet": packet,
    }


def filing_defs() -> dict[str, Any]:
    metadata = obj({
        "filing_event_id": UUID,
        "ticker": TICKER20,
        "cik": string(maximum=10, minimum=1, pattern=r"^[0-9]{1,10}$"),
        "accession_number": string(maximum=20, minimum=20, pattern=r"^[0-9]{10}-[0-9]{2}-[0-9]{6}$"),
        "form_type": FORM_TYPE,
        "filing_date": DATE,
        "report_date": nullable(DATE),
        "accepted_at": nullable(DATETIME),
        "sec_items": array(string(maximum=32, minimum=1), 128),
        "event_tags": array(string(maximum=128, minimum=1), 128),
        "filing_detail_url": HTTPS_URL,
        "primary_document_url": nullable(HTTPS_URL),
        "enrichment_status": string(maximum=13, enum=["not_requested", "queued", "ready", "failed"]),
    })
    section = obj({
        "key": string(maximum=120, minimum=1),
        "kind": string(maximum=19, enum=["sec_item", "document_section", "primary_document"]),
        "heading": string(maximum=1_000),
        "itemCode": nullable(string(maximum=32, minimum=1)),
        "confidence": string(maximum=9, enum=["exact", "heuristic", "fallback"]),
        "text_length": integer(0, 3_000_000),
    })
    document = obj({
        "document_key": string(maximum=120, minimum=12, pattern=r"^document:[A-Za-z0-9_-]{22}$"),
        "document_name": string(maximum=1_000, minimum=1),
        "document_type": nullable(string(maximum=1_000, minimum=1)),
        "description": nullable(string(maximum=1_000, minimum=1)),
        "document_url": HTTPS_URL,
        "sequence": nullable(integer(0, 1_000_000)),
        "size_bytes": nullable(integer(0, 1_000_000_000_000)),
        "is_primary": {"type": "boolean"},
        "recommendation_score": integer(0, 100),
        "readable": {"type": "boolean"},
    })
    citation = obj({
        "source": string(maximum=9, const="SEC EDGAR"),
        "filing_event_id": UUID,
        "accession_number": string(maximum=20, minimum=20, pattern=r"^[0-9]{10}-[0-9]{2}-[0-9]{6}$"),
        "filing_detail_url": HTTPS_URL,
        "document_url": HTTPS_URL,
        "section_key": string(maximum=120, minimum=1),
        "section_heading": string(maximum=1_200),
        "extraction": string(maximum=23, enum=["deterministic_html", "stored_structured_form4"]),
        "fetched_at": DATETIME,
        "document_key": string(maximum=120, minimum=12),
        "document_name": string(maximum=1_000, minimum=1),
        "document_type": nullable(string(maximum=1_000, minimum=1)),
    }, required=[
        "source", "filing_event_id", "accession_number", "filing_detail_url",
        "document_url", "section_key", "section_heading", "extraction", "fetched_at",
    ])
    fact = obj({
        "fact_ko": string(maximum=260, minimum=1),
        "section_key": string(maximum=180, minimum=1),
        "source_quote": string(maximum=700, minimum=1),
        "section_heading": string(maximum=300, minimum=1),
        "source_document_url": HTTPS_URL,
    })
    brief = obj({
        "filing_event_id": UUID,
        "locale": string(maximum=5, const="ko-KR"),
        "headline_ko": string(maximum=240, minimum=1),
        "summary_sentences": array(string(maximum=340, minimum=1), 2, 1),
        "facts": array(ref("filing_brief_fact"), 4, 1),
        "question_suggestions": array(string(maximum=220, minimum=1), 2),
        "uncertainty_notes": array(string(maximum=240, minimum=1), 3),
        "source_document_url": HTTPS_URL,
        "source_section_keys": array(string(maximum=180, minimum=1), 4, 1),
        "source_truncated": {"type": "boolean"},
        "generator_model": string(maximum=160, minimum=1),
        "prompt_version": string(maximum=80, minimum=1),
        "schema_version": integer(1, 1_000_000),
        "generated_at": DATETIME,
    })
    owner = obj({
        "filing_event_id": UUID,
        "reporting_owner_index": integer(0, 1_000_000),
        "owner_cik": nullable(string(maximum=10, minimum=1, pattern=r"^[0-9]{1,10}$")),
        "owner_name": string(maximum=1_000, minimum=1),
        "is_director": {"type": "boolean"},
        "is_officer": {"type": "boolean"},
        "is_ten_percent_owner": {"type": "boolean"},
        "is_other": {"type": "boolean"},
        "officer_title": nullable(string(maximum=1_000, minimum=1)),
        "created_at": DATETIME,
        "updated_at": DATETIME,
    })
    transaction = obj({
        "id": UUID,
        "filing_event_id": UUID,
        "reporting_owner_index": integer(0, 1_000_000),
        "transaction_index": integer(0, 1_000_000),
        "security_kind": string(maximum=14, enum=["non_derivative", "derivative"]),
        "security_title": string(maximum=1_000, minimum=1),
        "transaction_date": nullable(DATE),
        "transaction_code": nullable(string(maximum=16, minimum=1)),
        "equity_swap_involved": nullable({"type": "boolean"}),
        "transaction_shares": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "transaction_price_per_share": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "acquired_disposed_code": nullable(string(maximum=1, enum=["A", "D"])),
        "shares_owned_following": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "direct_indirect_code": nullable(string(maximum=1, enum=["D", "I"])),
        "nature_of_ownership": nullable(string(maximum=4_000, minimum=1)),
        "reported_transaction_value": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "reported_value_basis": string(maximum=12, enum=["p_or_s", "not_reported"]),
        "footnotes": ref("json_value"),
        "created_at": DATETIME,
        "updated_at": DATETIME,
    })
    summary = obj({
        "filing_event_id": UUID,
        "reporting_owner_count": integer(0, 1_000_000),
        "transaction_count": integer(0, 1_000_000),
        "reported_purchase_count": integer(0, 1_000_000),
        "reported_sale_count": integer(0, 1_000_000),
        "reported_purchase_shares": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "reported_sale_shares": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "reported_purchase_value": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "reported_sale_value": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "net_reported_shares": nullable(number(-MAX_NUMBER, MAX_NUMBER)),
        "tenb5_one_status": string(maximum=7, enum=["yes", "no", "unknown"]),
        "has_reported_open_market_trade": {"type": "boolean"},
        "source_xml_version": nullable(string(maximum=160, minimum=1)),
        "created_at": DATETIME,
        "updated_at": DATETIME,
    })
    return {
        "json_value": JSON_VALUE,
        "filing_metadata": metadata,
        "filing_section": section,
        "filing_document": document,
        "filing_citation": citation,
        "filing_brief_fact": fact,
        "filing_brief": brief,
        "form4_owner": owner,
        "form4_transaction": transaction,
        "form4_summary": summary,
    }


def root(contract_id: str, body: dict[str, Any], defs: dict[str, Any] | None = None) -> dict[str, Any]:
    result = {"$id": f"urn:krw-agent:contract:{contract_id}", "$schema": DIALECT, **body}
    if defs:
        result["$defs"] = defs
    return result


def id_input(contract_id: str) -> dict[str, Any]:
    return root(contract_id, obj({"filing_event_id": UUID}))


def build_contracts() -> dict[str, dict[str, Any]]:
    feed_get = feed_defs(2)
    feed_list = feed_defs(0)
    feed_context = feed_defs(3)
    filing = filing_defs()
    contracts: dict[str, dict[str, Any]] = {}
    contracts["krw-feed-get-items-input/v1"] = root(
        "krw-feed-get-items-input/v1",
        obj({
            "issue_ids": array(UUID, 8, 1),
            "include_sources": {"type": "boolean"},
        }, required=["issue_ids"]),
    )
    contracts["krw-feed-get-items-result/v1"] = root(
        "krw-feed-get-items-result/v1",
        obj({
            "environment": ENVIRONMENT,
            "items": array(ref("feed_issue"), 8),
            "missing_issue_ids": array(UUID, 8, unique=True),
        }), feed_get,
    )
    contracts["krw-feed-list-items-input/v1"] = root(
        "krw-feed-list-items-input/v1",
        obj({
            "tickers": array(TICKER16_INPUT, 5),
            "published_after": DATETIME,
            "limit": integer(1, 20),
            "cursor": string(maximum=6, minimum=1, pattern=r"^[0-9]{1,6}$"),
        }, required=[]),
    )
    contracts["krw-feed-list-items-result/v1"] = root(
        "krw-feed-list-items-result/v1",
        obj({
            "environment": ENVIRONMENT,
            "items": array(ref("feed_issue"), 20),
            "pagination": obj({
                "limit": integer(1, 20),
                "next_cursor": nullable(string(maximum=6, minimum=1, pattern=r"^[0-9]{1,6}$")),
                "has_more": {"type": "boolean"},
            }),
        }), feed_list,
    )
    contracts["krw-feed-context-input/v2"] = root(
        "krw-feed-context-input/v2",
        obj({
            "issue_ids": array(UUID, 8, 1),
            "tickers": array(TICKER16_INPUT, 5),
            "max_posts_per_issue": integer(1, 3),
        }, required=["issue_ids"]),
    )
    contracts["krw-feed-context/v2"] = root(
        "krw-feed-context/v2",
        obj({
            "version": string(maximum=19, const="krw-feed-context/v2"),
            "environment": ENVIRONMENT,
            "context_hash": SHA256,
            "requested_issue_ids": array(UUID, 8, 1, unique=True),
            "items": array(ref("feed_issue"), 8),
            "tickers": array(TICKER16, 5, unique=True),
            "company_groups": array(obj({
                "ticker": TICKER16,
                "issue_ids": array(UUID, 8, unique=True),
            }), 5),
            "gaps": array(obj({
                "issue_id": UUID,
                "code": string(maximum=26, const="not_found_or_not_published"),
            }), 8),
            "source_materials": array(ref("feed_source_material"), 24),
            "research_packets": array(ref("feed_research_packet"), 8),
        }), feed_context,
    )

    contracts["krw-filing-get-input/v1"] = id_input("krw-filing-get-input/v1")
    contracts["krw-filing-metadata/v1"] = root(
        "krw-filing-metadata/v1", ref("filing_metadata"), filing,
    )
    contracts["krw-filing-brief-input/v1"] = id_input("krw-filing-brief-input/v1")
    contracts["krw-filing-brief-result/v1"] = root(
        "krw-filing-brief-result/v1",
        obj({
            "filing": ref("filing_metadata"),
            "status": string(maximum=13, enum=["ready", "not_available"]),
            "brief": nullable(ref("filing_brief")),
            "machine_generated": {"const": True, "type": "boolean"},
            "note": string(maximum=1_000, minimum=1),
        }), filing,
    )
    contracts["krw-filing-search-input/v1"] = root(
        "krw-filing-search-input/v1",
        obj({"ticker": TICKER20_INPUT, "form_type": FORM_TYPE, "limit": integer(1, 25)}, required=["ticker"]),
    )
    contracts["krw-filing-search-result/v1"] = root(
        "krw-filing-search-result/v1", array(ref("filing_metadata"), 25), filing,
    )
    contracts["krw-filing-sections-input/v1"] = id_input("krw-filing-sections-input/v1")
    contracts["krw-filing-sections-result/v1"] = root(
        "krw-filing-sections-result/v1",
        obj({
            "filing": ref("filing_metadata"),
            "sections": array(ref("filing_section"), 512, 1),
            "document_url": HTTPS_URL,
            "fetched_at": DATETIME,
            "document_truncated": {"type": "boolean"},
        }), filing,
    )
    contracts["krw-filing-read-section-input/v1"] = root(
        "krw-filing-read-section-input/v1",
        obj({
            "filing_event_id": UUID,
            "section_key": string(maximum=120, minimum=1),
            "offset": integer(0, 3_000_000),
            "max_chars": integer(1_000, 120_000),
        }, required=["filing_event_id", "section_key"]),
    )
    contracts["krw-filing-read-section-result/v1"] = root(
        "krw-filing-read-section-result/v1",
        obj({
            "filing": ref("filing_metadata"),
            "section": ref("filing_section"),
            "text": string(maximum=120_000),
            "offset": integer(0, 3_000_000),
            "next_offset": nullable(integer(0, 3_000_000)),
            "document_truncated": {"type": "boolean"},
            "citation": ref("filing_citation"),
        }), filing,
    )
    contracts["krw-filing-documents-input/v1"] = id_input("krw-filing-documents-input/v1")
    contracts["krw-filing-documents-result/v1"] = root(
        "krw-filing-documents-result/v1",
        obj({
            "filing": ref("filing_metadata"),
            "documents": array(ref("filing_document"), 256, 1),
            "fetched_at": DATETIME,
        }), filing,
    )
    contracts["krw-filing-read-document-input/v1"] = root(
        "krw-filing-read-document-input/v1",
        obj({
            "filing_event_id": UUID,
            "document_key": string(maximum=120, minimum=12),
            "offset": integer(0, 3_000_000),
            "max_chars": integer(1_000, 120_000),
        }, required=["filing_event_id", "document_key"]),
    )
    contracts["krw-filing-read-document-result/v1"] = root(
        "krw-filing-read-document-result/v1",
        obj({
            "filing": ref("filing_metadata"),
            "document": ref("filing_document"),
            "text": string(maximum=120_000),
            "offset": integer(0, 3_000_000),
            "next_offset": nullable(integer(0, 3_000_000)),
            "document_truncated": {"type": "boolean"},
            "citation": ref("filing_citation"),
        }), filing,
    )
    contracts["krw-form4-transactions-input/v1"] = id_input("krw-form4-transactions-input/v1")
    contracts["krw-form4-transactions-result/v1"] = root(
        "krw-form4-transactions-result/v1",
        obj({
            "filing": ref("filing_metadata"),
            "summary": nullable(ref("form4_summary")),
            "reporting_owners": array(ref("form4_owner"), 128),
            "transactions": array(ref("form4_transaction"), 4_096),
            "source": string(maximum=29, const="stored_structured_form4_facts"),
            "citation": ref("filing_citation"),
        }), filing,
    )
    return contracts


def fixture_values() -> dict[str, Any]:
    filing_id = "11111111-1111-4111-8111-111111111111"
    issue_id = "22222222-2222-4222-8222-222222222222"
    metadata = {
        "filing_event_id": filing_id, "ticker": "ACME", "cik": "1234567",
        "accession_number": "0001234567-26-000001", "form_type": "8-K",
        "filing_date": "2026-08-01", "report_date": None,
        "accepted_at": "2026-08-01T12:00:00Z", "sec_items": ["2.02"],
        "event_tags": ["earnings"],
        "filing_detail_url": "https://www.sec.gov/Archives/edgar/data/1234567/000123456726000001/index.html",
        "primary_document_url": "https://www.sec.gov/Archives/edgar/data/1234567/000123456726000001/acme.htm",
        "enrichment_status": "ready",
    }
    issue = {
        "id": issue_id, "event_key": "acme-event", "title_ko": "제목", "summary_ko": "요약",
        "impact_path": [], "confirmed_facts": [], "unconfirmed_facts": [], "status_labels": [],
        "source_summary": {}, "source_count": 1, "market_score": 1, "risk_level": "medium",
        "first_observed_at": None, "published_at": "2026-08-01T12:00:00Z",
        "updated_at": None, "entities": [], "source_posts": [],
    }
    citation = {
        "source": "SEC EDGAR", "filing_event_id": filing_id,
        "accession_number": metadata["accession_number"],
        "filing_detail_url": metadata["filing_detail_url"],
        "document_url": metadata["primary_document_url"], "section_key": "item:2.02",
        "section_heading": "Item 2.02", "extraction": "deterministic_html",
        "fetched_at": "2026-08-01T12:01:00Z",
    }
    section = {"key": "item:2.02", "kind": "sec_item", "heading": "Item 2.02", "itemCode": "2.02", "confidence": "exact", "text_length": 4}
    primary_section = {"key": "document:primary", "kind": "primary_document", "heading": "Primary filing document", "itemCode": None, "confidence": "fallback", "text_length": 4}
    document = {"document_key": "document:abcdefghijklmnopqrstuv", "document_name": "acme.htm", "document_type": "8-K", "description": "Primary", "document_url": metadata["primary_document_url"], "sequence": 1, "size_bytes": 1000, "is_primary": True, "recommendation_score": 60, "readable": True}
    return {
        "krw-feed-get-items-input/v1": {"issue_ids": [issue_id]},
        "krw-feed-get-items-result/v1": {"environment": "prod", "items": [issue], "missing_issue_ids": []},
        "krw-feed-list-items-input/v1": {"tickers": ["ACME"], "limit": 10},
        "krw-feed-list-items-result/v1": {"environment": "prod", "items": [issue], "pagination": {"limit": 10, "next_cursor": None, "has_more": False}},
        "krw-feed-context-input/v2": {"issue_ids": [issue_id], "tickers": ["ACME"]},
        "krw-feed-context/v2": {"version": "krw-feed-context/v2", "environment": "prod", "context_hash": f"sha256:{'0' * 64}", "requested_issue_ids": [issue_id], "items": [issue], "tickers": ["ACME"], "company_groups": [{"ticker": "ACME", "issue_ids": []}], "gaps": [], "source_materials": [], "research_packets": []},
        "krw-filing-get-input/v1": {"filing_event_id": filing_id},
        "krw-filing-metadata/v1": metadata,
        "krw-filing-brief-input/v1": {"filing_event_id": filing_id},
        "krw-filing-brief-result/v1": {"filing": metadata, "status": "not_available", "brief": None, "machine_generated": True, "note": "Not available"},
        "krw-filing-search-input/v1": {"ticker": "ACME", "limit": 10},
        "krw-filing-search-result/v1": [metadata],
        "krw-filing-sections-input/v1": {"filing_event_id": filing_id},
        "krw-filing-sections-result/v1": {"filing": metadata, "sections": [primary_section, section], "document_url": metadata["primary_document_url"], "fetched_at": "2026-08-01T12:01:00Z", "document_truncated": False},
        "krw-filing-read-section-input/v1": {"filing_event_id": filing_id, "section_key": "item:2.02", "offset": 0, "max_chars": 24000},
        "krw-filing-read-section-result/v1": {"filing": metadata, "section": section, "text": "text", "offset": 0, "next_offset": None, "document_truncated": False, "citation": citation},
        "krw-filing-documents-input/v1": {"filing_event_id": filing_id},
        "krw-filing-documents-result/v1": {"filing": metadata, "documents": [document], "fetched_at": "2026-08-01T12:01:00Z"},
        "krw-filing-read-document-input/v1": {"filing_event_id": filing_id, "document_key": document["document_key"], "offset": 0, "max_chars": 24000},
        "krw-filing-read-document-result/v1": {"filing": metadata, "document": document, "text": "text", "offset": 0, "next_offset": None, "document_truncated": False, "citation": {**citation, "section_key": document["document_key"], "document_key": document["document_key"], "document_name": document["document_name"], "document_type": document["document_type"]}},
        "krw-form4-transactions-input/v1": {"filing_event_id": filing_id},
        "krw-form4-transactions-result/v1": {"filing": {**metadata, "form_type": "4"}, "summary": None, "reporting_owners": [], "transactions": [], "source": "stored_structured_form4_facts", "citation": {**citation, "section_key": "form4:transactions", "section_heading": "Structured Form 4 insider transactions", "extraction": "stored_structured_form4"}},
    }


def constant_name(contract_id: str) -> str:
    return contract_id.upper().replace("-", "_").replace("/", "_")


def export(source_root: Path, output_root: Path) -> None:
    for relative, expected in SOURCE_PINS.items():
        path = source_root / relative
        observed = hashlib.sha256(path.read_bytes()).hexdigest()
        if observed != expected:
            raise SystemExit(f"authority drift: {relative}: expected {expected}, observed {observed}")

    contracts = build_contracts()
    fixtures = fixture_values()
    if contracts.keys() != fixtures.keys():
        raise SystemExit("every contract must have one positive conformance fixture")
    schemas_dir = output_root / "schemas"
    schemas_dir.mkdir(parents=True, exist_ok=True)

    contract_manifest: dict[str, Any] = {}
    bindings: list[str] = ["// @generated by scripts/export_front_read_contracts.py; do not edit."]
    for contract_id, schema in sorted(contracts.items()):
        filename = contract_id.replace("/", "-") + ".json"
        data = canonical(schema)
        (schemas_dir / filename).write_bytes(data)
        digest = sha(data)
        contract_manifest[contract_id] = {
            "authority_kind": "typescript_zod_and_return_type",
            "authority_ref": "krw-ontology-front read-only MCP snapshot",
            "schema_path": f"schemas/{filename}",
            "schema_sha256": digest,
            "semantic_validation": "bounded_rust_typed_validator",
        }
        name = constant_name(contract_id)
        bindings.append(f'pub const {name}: &str = "{contract_id}";')
        bindings.append(f'pub const {name}_SCHEMA_SHA256: &str = "{digest}";')

    authority = {
        "kind": "krw-ontology-front/typescript-zod-snapshot",
        "repository": "krw-ontology-front",
        "source_sha256": {path: f"sha256:{digest}" for path, digest in sorted(SOURCE_PINS.items())},
    }
    authority_hash = sha(canonical(authority))
    bundle = {
        "format": FORMAT,
        "json_schema_dialect": DIALECT,
        "authority_sha256": authority_hash,
        "notice": "TypeScript/Zod and explicit return types in krw-ontology-front remain authoritative; this is a fail-closed deployment snapshot.",
        "contracts": {key: contracts[key] for key in sorted(contracts)},
    }
    bundle_bytes = canonical(bundle)
    (output_root / "schema-bundle.json").write_bytes(bundle_bytes)

    vectors: list[dict[str, Any]] = []
    for contract_id, value in sorted(fixtures.items()):
        vectors.append({"contract_id": contract_id, "name": "minimal_valid", "valid": True, "value": value})
        if isinstance(value, dict):
            invalid = {**value, "unexpected": True}
        else:
            invalid = {"unexpected": value}
        vectors.append({"contract_id": contract_id, "name": "reject_unknown_top_level", "valid": False, "value": invalid})
    vectors_doc = {
        "format": "krw-front-contract-conformance/v1",
        "authority_sha256": authority_hash,
        "vectors": vectors,
    }
    vectors_bytes = canonical(vectors_doc)
    (output_root / "conformance-vectors.json").write_bytes(vectors_bytes)

    manifest = {
        "format": FORMAT,
        "authority": authority,
        "authority_sha256": authority_hash,
        "schema_bundle": {"path": "schema-bundle.json", "sha256": sha(bundle_bytes)},
        "conformance_vectors": {"path": "conformance-vectors.json", "sha256": sha(vectors_bytes)},
        "contracts": contract_manifest,
    }
    manifest_bytes = canonical(manifest)
    (output_root / "manifest.json").write_bytes(manifest_bytes)
    manifest_hash = sha(manifest_bytes)
    (output_root / "manifest.sha256").write_text(
        f"{manifest_hash}  manifest.json\n", encoding="utf-8"
    )
    bindings.append(f'pub const FRONT_GENERATED_MANIFEST_SHA256: &str = "{manifest_hash}";')
    (output_root / "bindings.rs").write_text("\n".join(bindings) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--output-root", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    source_root = args.source_root.resolve()
    output_root = args.output_root.resolve()
    if not args.check:
        export(source_root, output_root)
        return
    with tempfile.TemporaryDirectory(prefix="krw-front-contract-check-") as temporary:
        generated = Path(temporary)
        export(source_root, generated)
        expected_files = {
            path.relative_to(output_root): path.read_bytes()
            for path in output_root.rglob("*")
            if path.is_file()
        }
        generated_files = {
            path.relative_to(generated): path.read_bytes()
            for path in generated.rglob("*")
            if path.is_file()
        }
        if expected_files != generated_files:
            missing = sorted(str(path) for path in generated_files.keys() - expected_files.keys())
            stale = sorted(
                str(path)
                for path in expected_files.keys() & generated_files.keys()
                if expected_files[path] != generated_files[path]
            )
            extra = sorted(str(path) for path in expected_files.keys() - generated_files.keys())
            raise SystemExit(
                f"front contract artifacts are stale: missing={missing}, changed={stale}, extra={extra}"
            )


if __name__ == "__main__":
    main()
