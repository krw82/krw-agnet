"""Translate engine visualization artifacts into workspace widget payloads.

The engine compiles presentation packs into deterministic
``krw-visualization`` artifacts (format schema v4): one artifact carries a
title plus ``views`` (one chart per view with typed series and
evidence-referenced points). Widgets are how those charts survive as
dashboard assets instead of sinking into chat text (roadmap axis 3).

The workspace owns the widget runtime — we do not reimplement widget SQL
or a browser bridge. This module only performs the pure, testable
translation into ``create_widget`` function-call payloads:

- one widget per artifact view (``view_id`` becomes the widget id);
- deterministic widget UUIDs: a stable hash of ``origin + widget_id`` so
  republishing the same view of the same run is idempotent on the
  workspace side (the reference workspace derives ids the same way from
  origin + widget id; ours is gateway-local but serves the same purpose);
- provenance is preserved structurally: every data source carries the
  artifact format version and the view's evidence refs as artifact-kind
  sources — never fabricated web URLs;
- chart types map conservatively; unknown engine chart types are passed
  through verbatim rather than guessed.

Output contract: ``krw-gateway/create-widget/v1`` payloads wrapped in the
workspace function-call envelope (``{"name": "create_widget", "arguments":
...}``). The host adapter forwards these over the workspace SSE channel;
if the deployed workspace expects slightly different argument field names,
that mapping belongs to one explicit adaptation point here, not to the
engine.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any

ARTIFACT_FORMAT = "krw-visualization"
GATEWAY_PAYLOAD_FORMAT = "krw-gateway/create-widget/v1"
CREATE_WIDGET_FUNCTION = "create_widget"

#: Engine chart_type -> workspace widget_type. Conservative: structural
#: chart families only; anything unmapped passes through verbatim.
CHART_TYPE_MAP: dict[str, str] = {
    "line": "line_chart",
    "bar": "bar_chart",
    "area": "area_chart",
    "scatter": "scatter_chart",
    "waterfall": "waterfall_chart",
    "table": "table",
}


class WidgetPublishError(ValueError):
    """The artifact is not a publishable krw-visualization pack."""


def widget_uuid(origin: str, widget_id: str) -> str:
    """Deterministic 64-bit id for one widget of one origin.

    blake2b (8-byte digest) keeps this stdlib-only while remaining
    collision-safe for widget identity purposes. Same inputs always yield
    the same id, so republishing is idempotent.
    """

    material = f"{origin}\x1f{widget_id}".encode("utf-8")
    return hashlib.blake2b(material, digest_size=8).hexdigest()


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise WidgetPublishError(message)


def publishable_widgets(
    artifact: dict[str, Any],
    *,
    origin: str,
) -> list[dict[str, Any]]:
    """Translate one krw-visualization artifact into widget payloads.

    Returns one ``krw-gateway/create-widget/v1`` payload per view. Raises
    :class:`WidgetPublishError` when the artifact is not a v4 presentation
    pack (wrong/missing format, no views, or a view without series data)
    — an honest rejection instead of an empty widget.
    """

    _require(isinstance(artifact, dict), "artifact must be an object")
    views = artifact.get("views")
    # The engine (crates/krw-presentation lib.rs) emits "artifact_format";
    # "format" is accepted for backwards compatibility with older packs. No
    # masking default: a missing or wrong format key is an honest rejection,
    # never a silent pass.
    format_key = artifact.get("artifact_format") or artifact.get("format")
    _require(
        format_key == ARTIFACT_FORMAT,
        f"unsupported artifact format: {format_key!r}",
    )
    _require(isinstance(views, list) and bool(views), "artifact has no views")
    _require(bool(origin), "origin (run identity) is required")

    provenance = artifact.get("provenance") or {}
    artifact_evidence_refs = list(provenance.get("evidence_refs") or [])

    payloads: list[dict[str, Any]] = []
    for view in views:
        _require(isinstance(view, dict), "view must be an object")
        view_id = view.get("view_id")
        _require(isinstance(view_id, str) and bool(view_id), "view has no view_id")
        series = view.get("series")
        _require(
            isinstance(series, list) and bool(series),
            f"view {view_id} has no series data",
        )
        chart_type = view.get("chart_type")
        _require(isinstance(chart_type, str) and bool(chart_type), "view has no chart_type")

        widget_id = f"krw-{view_id}"
        view_evidence_refs = sorted(
            {
                str(point.get("evidence_ref"))
                for one_series in series
                if isinstance(one_series, dict)
                for point in one_series.get("points", [])
                if isinstance(point, dict) and point.get("evidence_ref")
            }
            | set(artifact_evidence_refs)
        )

        payloads.append(
            {
                "format": GATEWAY_PAYLOAD_FORMAT,
                "widget": {
                    "widget_id": widget_id,
                    "uuid": widget_uuid(origin, widget_id),
                    "title": view.get("title") or artifact.get("title") or widget_id,
                    "widget_type": CHART_TYPE_MAP.get(chart_type, chart_type),
                    "unit": view.get("unit"),
                },
                "data_sources": [
                    {
                        "kind": "artifact",
                        "format": f"{ARTIFACT_FORMAT}/v4",
                        "view_id": view_id,
                        "series": series,
                        "evidence_refs": view_evidence_refs,
                    }
                ],
                # Advisory honesty: engine charts are derived presentation
                # over the evidence ledger, not new factual claims.
                "usage_note": (
                    "Deterministic engine visualization. Advisory presentation "
                    "of ledger-backed observations; not filing evidence."
                ),
            }
        )
    return payloads


def to_openbb_function_call(payload: dict[str, Any]) -> dict[str, Any]:
    """Wrap one gateway payload in the workspace function-call envelope."""

    _require(
        payload.get("format") == GATEWAY_PAYLOAD_FORMAT,
        "payload is not a krw-gateway/create-widget/v1 document",
    )
    return {
        "name": CREATE_WIDGET_FUNCTION,
        "arguments": json.loads(json.dumps(payload)),  # deep copy, JSON-safe
    }
