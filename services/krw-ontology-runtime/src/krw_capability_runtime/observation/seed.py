"""Observation series seed loader (P1).

The seed YAML is the single static description of the observation series
families: which provider series feed which canonical metric, in what unit and
frequency, under which provisional factor label.  The builder (B3) consumes
``load_series_seed()`` to plan collection; nothing here talks to a network.
Observation values are advisory_only research context: never filing evidence,
never strong-claim support, never recommendation or price-target grounds.
"""

from __future__ import annotations

from dataclasses import dataclass, fields
from pathlib import Path
from typing import Any, Mapping

# Provisional P1 factor labels.  The formal join to
# ontology/taxonomy/factors.yaml happens in P2; until then these labels are
# the closed vocabulary macro series may carry.
KNOWN_FACTOR_LABELS = frozenset(
    {
        "inflation",
        "interest_rate",
        "labor",
        "growth",
        "housing",
        "market_volatility",
    }
)

_KNOWN_DOMAINS = frozenset({"macro", "price", "valuation"})
_KNOWN_PROVIDERS = frozenset({"fred", "fmp", "polygon"})
_PLACEHOLDER_PROVIDER_SERIES_ID = "ticker"

_REPO_SEED_PATH = (
    Path(__file__).resolve().parents[3] / "ontology" / "observation" / "series_seed.yaml"
)
_PACKAGE_SEED_PATH = (
    Path(__file__).resolve().parents[1] / "resources" / "observation" / "series_seed.yaml"
)
# Capability-runtime resource copy: the import script lands the upstream
# ``ontology/`` tree at ``<package>/resources/ontology/`` (same convention as
# the metric dictionary resource), so the serving package resolves the seed
# from there.
_RUNTIME_RESOURCES_SEED_PATH = (
    Path(__file__).resolve().parents[1]
    / "resources"
    / "ontology"
    / "observation"
    / "series_seed.yaml"
)


@dataclass(frozen=True)
class SeriesDefinition:
    """One observation series family.

    series_key: stable internal key (``macro_<name>`` for FRED families,
        bare family name for per-ticker price/valuation templates).
    domain: ``macro`` | ``price`` | ``valuation``.
    provider: ``fred`` | ``fmp`` | ``polygon`` (vendor name never leaves the
        collection layer).
    provider_series_id: vendor-native identifier, or the ``ticker``
        placeholder for per-ticker templates (the builder substitutes the
        canonical ticker per instance).
    canonical_metric: canonical id from ontology/schema/metric_dictionary.yaml.
    unit: served unit (percent, USD_per_share, ratio, ...).
    frequency: daily | weekly | monthly | quarterly.
    adjustment: seasonal | saar | none.
    factor: provisional factor label for macro families; None for
        price/valuation families.
    ticker: equity ticker for per-ticker instances; None for macro series and
        for per-ticker family TEMPLATES.
    is_per_ticker: True when the entry models a per-ticker family template
        rather than one concrete series.
    """

    series_key: str
    domain: str
    provider: str
    provider_series_id: str
    canonical_metric: str
    unit: str
    frequency: str
    adjustment: str
    factor: str | None
    ticker: str | None = None
    is_per_ticker: bool = False


_SERIES_FIELDS = frozenset(field.name for field in fields(SeriesDefinition))
# series_key comes from the YAML mapping key; ticker/is_per_ticker default.
_REQUIRED_SERIES_FIELDS = tuple(
    field.name
    for field in fields(SeriesDefinition)
    if field.name not in {"series_key", "ticker", "is_per_ticker"}
)


def series_seed_path(path: Path | None = None) -> Path:
    """Resolve the seed YAML: explicit path, repo resource, then package resource."""
    candidates: list[Path] = []
    if path is not None:
        candidates.append(path)
    candidates.extend([_REPO_SEED_PATH, _RUNTIME_RESOURCES_SEED_PATH, _PACKAGE_SEED_PATH])
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return candidates[0]


def _string_value(series_key: str, field_name: str, value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"series {series_key!r}: field {field_name!r} must be a non-empty string")
    return value.strip()


def _definition_from_entry(series_key: str, entry: Mapping[str, Any]) -> SeriesDefinition:
    unknown = sorted(set(entry) - _SERIES_FIELDS)
    if unknown:
        raise ValueError(f"series {series_key!r}: unknown seed field(s) {unknown}")

    missing = [name for name in _REQUIRED_SERIES_FIELDS if name not in entry]
    if missing:
        raise ValueError(f"series {series_key!r}: missing required field(s) {missing}")

    domain = _string_value(series_key, "domain", entry["domain"])
    if domain not in _KNOWN_DOMAINS:
        raise ValueError(f"series {series_key!r}: unknown domain {domain!r}")

    provider = _string_value(series_key, "provider", entry["provider"])
    if provider not in _KNOWN_PROVIDERS:
        raise ValueError(f"series {series_key!r}: unknown provider {provider!r}")

    factor = entry.get("factor")
    if factor is not None:
        factor = _string_value(series_key, "factor", factor)
    if domain == "macro" and factor not in KNOWN_FACTOR_LABELS:
        raise ValueError(
            f"series {series_key!r}: macro factor {factor!r} is not a known factor label"
        )
    if domain != "macro" and factor is not None:
        raise ValueError(f"series {series_key!r}: only macro series carry a factor label")

    ticker = entry.get("ticker")
    if ticker is not None:
        ticker = _string_value(series_key, "ticker", ticker)
    is_per_ticker = bool(entry.get("is_per_ticker", False))
    if is_per_ticker and ticker is not None:
        raise ValueError(
            f"series {series_key!r}: per-ticker template must keep ticker null "
            "(the builder instantiates concrete tickers)"
        )
    if not is_per_ticker and ticker is not None:
        raise ValueError(
            f"series {series_key!r}: a concrete ticker requires is_per_ticker semantics"
        )

    provider_series_id = _string_value(
        series_key, "provider_series_id", entry["provider_series_id"]
    )
    if provider_series_id == _PLACEHOLDER_PROVIDER_SERIES_ID and not is_per_ticker:
        raise ValueError(
            f"series {series_key!r}: the {_PLACEHOLDER_PROVIDER_SERIES_ID!r} "
            "provider_series_id placeholder is reserved for per-ticker templates"
        )

    return SeriesDefinition(
        series_key=series_key,
        domain=domain,
        provider=provider,
        provider_series_id=provider_series_id,
        canonical_metric=_string_value(series_key, "canonical_metric", entry["canonical_metric"]),
        unit=_string_value(series_key, "unit", entry["unit"]),
        frequency=_string_value(series_key, "frequency", entry["frequency"]),
        adjustment=_string_value(series_key, "adjustment", entry["adjustment"]),
        factor=factor,
        ticker=ticker,
        is_per_ticker=is_per_ticker,
    )


def load_series_seed(path: Path | None = None) -> dict[str, SeriesDefinition]:
    """Load and validate the P1 observation series seed.

    Returns a mapping of series_key to SeriesDefinition.  Raises ValueError on
    malformed entries so a bad seed fails at build time, never silently.
    """

    import yaml

    resolved = series_seed_path(path)
    payload = yaml.safe_load(resolved.read_text()) or {}
    if not isinstance(payload, Mapping):
        raise ValueError(f"series seed must be a mapping: {resolved}")
    schema_version = payload.get("schema_version")
    if not isinstance(schema_version, str) or not schema_version.strip():
        raise ValueError(f"series seed is missing its schema_version: {resolved}")

    raw_series = payload.get("series")
    if not isinstance(raw_series, Mapping) or not raw_series:
        raise ValueError(f"series seed has no series entries: {resolved}")

    definitions: dict[str, SeriesDefinition] = {}
    for raw_key, raw_entry in raw_series.items():
        series_key = str(raw_key or "").strip()
        if not series_key:
            raise ValueError(f"series seed contains an empty series_key: {resolved}")
        if not isinstance(raw_entry, Mapping):
            raise ValueError(f"series {series_key!r}: entry must be a mapping")
        definitions[series_key] = _definition_from_entry(series_key, raw_entry)
    return definitions
