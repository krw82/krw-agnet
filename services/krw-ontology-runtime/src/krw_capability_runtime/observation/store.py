"""observations.sqlite store: schema, queries, and verification (schema v1).

The observation store is an immutable, advisory-only market/macro sidecar for
v3 releases.  Five tables carry the data (``series_catalog``, ``observations``
with vintage-aware primary keys, ``ohlcv_observations``,
``observation_revisions`` supersede edges, ``release_events``) plus one
metadata table, mirroring the chart_series sidecar conventions.

Doctrine invariants baked into this module:
- observation values are advisory_only research context: never filing
  evidence, never strong-claim support, never recommendation or price-target
  grounds;
- vendor names (provider columns) are internal collection plumbing only —
  every served payload below is vendor-scrubbed (``source_usage`` is always
  ``research_only`` and ``advisory_only`` is always true);
- supersede semantics: all vintages are retained as history; the latest
  ``result_time``/``vintage`` wins as the canonical current observation, and
  every non-first vintage carries an ``observation_revisions`` edge to its
  predecessor with ``revision_kind='revision'``.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

OBSERVATIONS_SCHEMA_VERSION = "krw-ontology-observations/v1"
OBSERVATIONS_BUILDER_VERSION = "observations-builder/v1"
OBSERVATIONS_RELATIVE_PATH = Path("indexes") / "observations.sqlite"

# The exact table set of schema v1: 5 data tables + the metadata table.
OBSERVATIONS_TABLES = (
    "series_catalog",
    "observations",
    "ohlcv_observations",
    "observation_revisions",
    "release_events",
)
OBSERVATIONS_METADATA_TABLE = "observation_metadata"
OBSERVATIONS_ALL_TABLES = frozenset(OBSERVATIONS_TABLES + (OBSERVATIONS_METADATA_TABLE,))

# Served payload format identifiers (B4/B5 contract).
MARKET_SERIES_FORMAT = "market-series/v1"
MACRO_SERIES_FORMAT = "macro-series/v1"
RELEASE_EVENTS_FORMAT = "release-events/v1"
SOURCE_USAGE_RESEARCH_ONLY = "research_only"

# Query bounds: a market series serves at most 260 points (≈ 1 year of daily
# bars or 20+ years of monthly periods) per request.
MAX_MARKET_PERIODS = 260
MAX_MACRO_POINTS = 260
MAX_RELEASE_EVENTS = 260

# PK sentinel: providers without a vintage concept (current-value feeds) get
# an empty-string vintage so the composite primary key stays NOT NULL-safe.
VINTAGE_NULL_SENTINEL = ""

# The ONE shared latest-vintage ordering. ``result_time`` (falling back to
# ``vintage``, then the PK sentinel) orders knowledge recency; ``vintage``
# breaks ties deterministically. This exact ordering must be used by BOTH the
# builder's supersede-chain construction and the query layer's latest
# resolution (see observation_sort_key) so builder artifacts
# (release_events.latest_value, revision edges) and served points can never
# crown different rows.
VINTAGE_ORDER_SQL = "COALESCE(result_time, vintage, '')"


def observation_sort_key(result_time: str | None, vintage: str | None) -> tuple[str, str]:
    """Single source of truth for "which vintage is latest".

    Mirrors ``ORDER BY COALESCE(result_time, vintage, '') DESC, vintage
    DESC`` in SQL: the Python twin is used by the builder (supersede chains,
    release events, OHLCV current view) and this module's queries, so a
    divergent (result_time, vintage) pair — e.g. an old vintage republished
    late — resolves identically on both sides.
    """

    resolved_vintage = vintage or VINTAGE_NULL_SENTINEL
    return (result_time or resolved_vintage or VINTAGE_NULL_SENTINEL, resolved_vintage)


def create_observations_schema(conn: sqlite3.Connection) -> None:
    """Create the schema-v1 tables (idempotent)."""

    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS observation_metadata (
            key TEXT PRIMARY KEY,
            value_json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS series_catalog (
            series_key TEXT PRIMARY KEY,
            domain TEXT NOT NULL,
            canonical_metric TEXT NOT NULL,
            unit TEXT NOT NULL,
            frequency TEXT NOT NULL,
            adjustment TEXT NOT NULL,
            factor TEXT,
            ticker TEXT,
            provider TEXT NOT NULL,
            provider_series_id TEXT NOT NULL,
            is_per_ticker INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL,
            observation_count INTEGER NOT NULL DEFAULT 0,
            first_phenomenon_time TEXT,
            last_phenomenon_time TEXT,
            last_result_time TEXT
        );

        CREATE TABLE IF NOT EXISTS observations (
            observation_id TEXT NOT NULL UNIQUE,
            series_key TEXT NOT NULL,
            phenomenon_time TEXT NOT NULL,
            value REAL NOT NULL,
            result_time TEXT,
            vintage TEXT NOT NULL DEFAULT '',
            provenance_json TEXT NOT NULL,
            PRIMARY KEY (series_key, phenomenon_time, vintage),
            FOREIGN KEY (series_key) REFERENCES series_catalog(series_key)
        );

        CREATE TABLE IF NOT EXISTS ohlcv_observations (
            ticker TEXT NOT NULL,
            trade_date TEXT NOT NULL,
            open REAL,
            high REAL,
            low REAL,
            close REAL NOT NULL,
            volume REAL,
            series_key TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            PRIMARY KEY (ticker, trade_date),
            FOREIGN KEY (series_key) REFERENCES series_catalog(series_key),
            FOREIGN KEY (observation_id) REFERENCES observations(observation_id)
        );

        CREATE TABLE IF NOT EXISTS observation_revisions (
            series_key TEXT NOT NULL,
            phenomenon_time TEXT NOT NULL,
            new_observation_id TEXT NOT NULL,
            prior_observation_id TEXT NOT NULL,
            revision_kind TEXT NOT NULL,
            PRIMARY KEY (new_observation_id, prior_observation_id),
            FOREIGN KEY (new_observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY (prior_observation_id) REFERENCES observations(observation_id)
        );

        CREATE TABLE IF NOT EXISTS release_events (
            series_key TEXT NOT NULL,
            canonical_metric TEXT NOT NULL,
            factor TEXT,
            phenomenon_time TEXT NOT NULL,
            release_time TEXT,
            latest_time TEXT,
            first_value REAL NOT NULL,
            latest_value REAL NOT NULL,
            revision_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (series_key, phenomenon_time),
            FOREIGN KEY (series_key) REFERENCES series_catalog(series_key)
        );

        CREATE INDEX IF NOT EXISTS idx_series_catalog_ticker_metric
            ON series_catalog(ticker, canonical_metric);
        CREATE INDEX IF NOT EXISTS idx_series_catalog_metric_domain
            ON series_catalog(canonical_metric, domain);
        CREATE INDEX IF NOT EXISTS idx_observations_series_time
            ON observations(series_key, phenomenon_time);
        CREATE INDEX IF NOT EXISTS idx_observations_series_result
            ON observations(series_key, phenomenon_time, result_time);
        CREATE INDEX IF NOT EXISTS idx_observation_revisions_series
            ON observation_revisions(series_key, phenomenon_time);
        CREATE INDEX IF NOT EXISTS idx_release_events_release_time
            ON release_events(release_time);
        CREATE INDEX IF NOT EXISTS idx_release_events_factor
            ON release_events(factor, release_time);
        """
    )


def observation_id_for(series_key: str, phenomenon_time: str, vintage: str | None) -> str:
    """Deterministic observation identity over the natural PK."""

    digest = hashlib.sha256(
        "|".join((series_key, phenomenon_time, vintage or VINTAGE_NULL_SENTINEL)).encode("utf-8")
    ).hexdigest()
    return f"obs:{digest[:20]}"


@dataclass(frozen=True)
class ObservationPoint:
    """One stored observation row; ``vintage`` is None when sentinel-backed."""

    observation_id: str
    series_key: str
    phenomenon_time: str
    value: float
    result_time: str | None
    vintage: str | None
    provenance: dict[str, Any]


@dataclass(frozen=True)
class RevisionEdge:
    """One supersede edge: ``new`` supersedes ``prior``."""

    series_key: str
    phenomenon_time: str
    new_observation_id: str
    prior_observation_id: str
    revision_kind: str


def _connect_read_only(path: Path | str) -> sqlite3.Connection:
    resolved = Path(path).expanduser().resolve()
    conn = sqlite3.connect(f"file:{resolved}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    return conn


def _bounded_count(requested: int, upper: int, *, floor: int = 1) -> int:
    try:
        value = int(requested)
    except (TypeError, ValueError):
        return upper
    return max(floor, min(value, upper))


def _currency_for_unit(unit: str | None) -> str | None:
    text = str(unit or "")
    if text.startswith("USD"):
        return "USD"
    if text.startswith(("KRW", "EUR", "JPY", "GBP")):
        return text[:3]
    return None


class ObservationsStore:
    """Read-side accessor over a built observations.sqlite store."""

    def __init__(self, path: Path | str) -> None:
        self.path = Path(path).expanduser().resolve()
        if not self.path.is_file():
            raise FileNotFoundError(f"observations store not found: {self.path}")
        self._conn = _connect_read_only(self.path)

    def close(self) -> None:
        self._conn.close()

    def __enter__(self) -> ObservationsStore:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    # -- metadata ----------------------------------------------------------

    def metadata(self) -> dict[str, Any]:
        rows = self._conn.execute(
            f"SELECT key, value_json FROM {OBSERVATIONS_METADATA_TABLE}"
        ).fetchall()
        metadata: dict[str, Any] = {}
        for row in rows:
            try:
                metadata[str(row["key"])] = json.loads(str(row["value_json"]))
            except (json.JSONDecodeError, TypeError):
                metadata[str(row["key"])] = row["value_json"]
        return metadata

    @property
    def fetched_at(self) -> str | None:
        value = self.metadata().get("built_at")
        return str(value) if value else None

    # -- vintage resolution --------------------------------------------------

    def latest_observation(self, series_key: str, phenomenon_time: str) -> ObservationPoint | None:
        """Return the latest-vintage observation for one phenomenon time."""

        row = self._conn.execute(
            f"""
            SELECT current.*
            FROM observations AS current
            WHERE current.series_key = ? AND current.phenomenon_time = ?
              AND current.observation_id = (
                  SELECT prior.observation_id
                  FROM observations AS prior
                  WHERE prior.series_key = current.series_key
                    AND prior.phenomenon_time = current.phenomenon_time
                  ORDER BY {VINTAGE_ORDER_SQL} DESC, prior.vintage DESC
                  LIMIT 1
              )
            """,
            (series_key, phenomenon_time),
        ).fetchone()
        return self._point_from_row(row) if row is not None else None

    def revisions_for(self, series_key: str, phenomenon_time: str) -> list[RevisionEdge]:
        """Supersede edges for one phenomenon time, oldest vintage first."""

        rows = self._conn.execute(
            """
            SELECT series_key, phenomenon_time, new_observation_id,
                   prior_observation_id, revision_kind
            FROM observation_revisions
            WHERE series_key = ? AND phenomenon_time = ?
            ORDER BY new_observation_id
            """,
            (series_key, phenomenon_time),
        ).fetchall()
        return [
            RevisionEdge(
                series_key=str(row["series_key"]),
                phenomenon_time=str(row["phenomenon_time"]),
                new_observation_id=str(row["new_observation_id"]),
                prior_observation_id=str(row["prior_observation_id"]),
                revision_kind=str(row["revision_kind"]),
            )
            for row in rows
        ]

    def ohlcv_row(self, ticker: str, trade_date: str) -> dict[str, Any] | None:
        row = self._conn.execute(
            """
            SELECT ticker, trade_date, open, high, low, close, volume,
                   series_key, observation_id
            FROM ohlcv_observations
            WHERE ticker = ? AND trade_date = ?
            """,
            (str(ticker).upper(), trade_date),
        ).fetchone()
        return dict(row) if row is not None else None

    # -- served payload queries ------------------------------------------------

    def query_market_series(
        self, ticker: str, canonical_metric: str, periods: int
    ) -> dict[str, Any]:
        """Serve a ``market-series/v1`` payload (latest vintage, bounded)."""

        bounded = _bounded_count(periods, MAX_MARKET_PERIODS)
        catalog_row = self._select_market_series_row(ticker, canonical_metric)
        if catalog_row is None:
            return self._empty_payload(
                MARKET_SERIES_FORMAT,
                ticker=ticker,
                canonical_metric=canonical_metric,
            )
        points = self._latest_vintage_points(str(catalog_row["series_key"]), bounded)
        payload: dict[str, Any] = {
            "format": MARKET_SERIES_FORMAT,
            "ticker": str(catalog_row["ticker"] or str(ticker).upper()),
            "canonical_metric": str(catalog_row["canonical_metric"]),
            "unit": str(catalog_row["unit"] or "") or None,
            "currency": _currency_for_unit(str(catalog_row["unit"] or "")),
            "status": str(catalog_row["status"]),
            "source_usage": SOURCE_USAGE_RESEARCH_ONLY,
            "advisory_only": True,
            "fetched_at": self.fetched_at,
            "as_of": points[-1]["date"] if points else None,
            "points": points,
        }
        if not points and payload["status"] == "available":
            payload["status"] = "no_data"
        return payload

    def query_macro_series(self, canonical_metric: str, limit: int) -> dict[str, Any]:
        """Serve a ``macro-series/v1`` payload (latest vintage, bounded)."""

        bounded = _bounded_count(limit, MAX_MACRO_POINTS)
        row = self._conn.execute(
            """
            SELECT * FROM series_catalog
            WHERE canonical_metric = ? AND domain = 'macro'
            ORDER BY status = 'available' DESC, series_key
            LIMIT 1
            """,
            (canonical_metric,),
        ).fetchone()
        if row is None:
            return self._empty_payload(MACRO_SERIES_FORMAT, canonical_metric=canonical_metric)
        points = self._latest_vintage_points(str(row["series_key"]), bounded)
        payload: dict[str, Any] = {
            "format": MACRO_SERIES_FORMAT,
            "series_key": str(row["series_key"]),
            "canonical_metric": str(row["canonical_metric"]),
            "unit": str(row["unit"] or "") or None,
            "frequency": str(row["frequency"]),
            "factor": row["factor"],
            "status": str(row["status"]),
            "source_usage": SOURCE_USAGE_RESEARCH_ONLY,
            "advisory_only": True,
            "fetched_at": self.fetched_at,
            "as_of": points[-1]["date"] if points else None,
            "points": points,
        }
        if not points and payload["status"] == "available":
            payload["status"] = "no_data"
        return payload

    def query_recent_releases(self, factor: str | None, limit: int) -> dict[str, Any]:
        """Serve recent release events (P1: release dates only, no surprise)."""

        bounded = _bounded_count(limit, MAX_RELEASE_EVENTS)
        where = "WHERE factor = ?" if factor else ""
        params: tuple[Any, ...] = (factor,) if factor else ()
        rows = self._conn.execute(
            f"""
            SELECT series_key, canonical_metric, factor, phenomenon_time,
                   release_time, latest_time, first_value, latest_value,
                   revision_count
            FROM release_events
            {where}
            ORDER BY COALESCE(release_time, '') DESC,
                     COALESCE(latest_time, '') DESC,
                     series_key DESC,
                     phenomenon_time DESC
            LIMIT ?
            """,
            (*params, bounded),
        ).fetchall()
        releases = [
            {
                "series_key": str(row["series_key"]),
                "canonical_metric": str(row["canonical_metric"]),
                "factor": row["factor"],
                "phenomenon_time": str(row["phenomenon_time"]),
                "release_time": row["release_time"],
                "latest_time": row["latest_time"],
                "first_value": float(row["first_value"]),
                "latest_value": float(row["latest_value"]),
                "revision_count": int(row["revision_count"]),
                "revised": int(row["revision_count"]) > 0,
                # P1 carries no consensus data; surprise stays reserved for P2.
                "surprise": None,
            }
            for row in rows
        ]
        return {
            "format": RELEASE_EVENTS_FORMAT,
            "factor": factor,
            "advisory_only": True,
            "source_usage": SOURCE_USAGE_RESEARCH_ONLY,
            "releases": releases,
        }

    # -- internals -------------------------------------------------------------

    def _select_market_series_row(self, ticker: str, canonical_metric: str) -> sqlite3.Row | None:
        rows = self._conn.execute(
            """
            SELECT * FROM series_catalog
            WHERE ticker = ? AND canonical_metric = ?
            ORDER BY status = 'available' DESC,
                     observation_count DESC,
                     series_key
            """,
            (str(ticker).upper(), canonical_metric),
        ).fetchall()
        return rows[0] if rows else None

    def _latest_vintage_points(self, series_key: str, limit: int) -> list[dict[str, Any]]:
        # Exactly one row per phenomenon_time: a correlated top-1 pick under
        # the shared vintage ordering (VINTAGE_ORDER_SQL, then vintage DESC).
        # A plain MAX(...) comparison would match every row sharing the max
        # key and emit duplicate same-date points with superseded values.
        rows = self._conn.execute(
            f"""
            SELECT current.*
            FROM observations AS current
            WHERE current.series_key = ?
              AND current.observation_id = (
                  SELECT prior.observation_id
                  FROM observations AS prior
                  WHERE prior.series_key = current.series_key
                    AND prior.phenomenon_time = current.phenomenon_time
                  ORDER BY {VINTAGE_ORDER_SQL} DESC, prior.vintage DESC
                  LIMIT 1
              )
            ORDER BY current.phenomenon_time DESC
            LIMIT ?
            """,
            (series_key, limit),
        ).fetchall()
        return [
            {
                "date": str(row["phenomenon_time"]),
                "value": float(row["value"]),
                "observation_id": str(row["observation_id"]),
            }
            for row in reversed(rows)
        ]

    def _empty_payload(
        self,
        format_id: str,
        *,
        ticker: str | None = None,
        canonical_metric: str | None = None,
    ) -> dict[str, Any]:
        # One stable shape for the no-data case: the union of the populated
        # market-series/v1 and macro-series/v1 keys, with unknown fields as
        # None, so downstream consumers never branch on payload shape.
        return {
            "format": format_id,
            "series_key": None,
            "ticker": ticker,
            "canonical_metric": canonical_metric,
            "unit": None,
            "currency": None,
            "frequency": None,
            "factor": None,
            "status": "no_data",
            "source_usage": SOURCE_USAGE_RESEARCH_ONLY,
            "advisory_only": True,
            "fetched_at": self.fetched_at,
            "as_of": None,
            "points": [],
        }

    def _point_from_row(self, row: sqlite3.Row) -> ObservationPoint:
        vintage = str(row["vintage"] or "")
        try:
            provenance = json.loads(str(row["provenance_json"]))
        except (json.JSONDecodeError, TypeError):
            provenance = {}
        return ObservationPoint(
            observation_id=str(row["observation_id"]),
            series_key=str(row["series_key"]),
            phenomenon_time=str(row["phenomenon_time"]),
            value=float(row["value"]),
            result_time=row["result_time"],
            vintage=vintage or None,
            provenance=provenance if isinstance(provenance, dict) else {},
        )


# ---------------------------------------------------------------------------
# Module-level query helpers (B4/B5 serving boundary): open, serve, close.
# ---------------------------------------------------------------------------


def query_market_series(
    path: Path | str, ticker: str, canonical_metric: str, periods: int = MAX_MARKET_PERIODS
) -> dict[str, Any]:
    with ObservationsStore(path) as store:
        return store.query_market_series(ticker, canonical_metric, periods)


def query_macro_series(
    path: Path | str, canonical_metric: str, limit: int = MAX_MACRO_POINTS
) -> dict[str, Any]:
    with ObservationsStore(path) as store:
        return store.query_macro_series(canonical_metric, limit)


def query_recent_releases(
    path: Path | str, factor: str | None = None, limit: int = MAX_RELEASE_EVENTS
) -> dict[str, Any]:
    with ObservationsStore(path) as store:
        return store.query_recent_releases(factor, limit)


# ---------------------------------------------------------------------------
# Verification (chart_series / agent_index verifier conventions).
# ---------------------------------------------------------------------------


def verify_observations_schema(path: Path | str, *, deep: bool = True) -> dict[str, Any]:
    """Verify a built observations store; hard-fails on any schema drift.

    Checks (mirroring verify_chart_series_index / verify_agent_index):
    - exact table set: the 5 data tables + observation_metadata, no extras;
    - schema/builder version constants match;
    - referential sanity: no observation references a missing series_catalog
      row, no revision edge references a missing observation, no release
      event references a missing series, no OHLCV row references a missing
      series/observation;
    - ``PRAGMA integrity_check``;
    - per-table row counts.
    """

    resolved = Path(path).expanduser().resolve()
    errors: list[str] = []
    counts: dict[str, int] = {}
    metadata: dict[str, Any] = {}
    tables: list[str] = []
    if not resolved.exists():
        return {
            "ok": False,
            "errors": ["observations_store_missing"],
            "path": str(resolved),
            "tables": tables,
            "metadata": metadata,
            "counts": counts,
            "schema_version": None,
            "verification_mode": "observations",
        }
    if not resolved.is_file():
        return {
            "ok": False,
            "errors": ["observations_store_not_file"],
            "path": str(resolved),
            "tables": tables,
            "metadata": metadata,
            "counts": counts,
            "schema_version": None,
            "verification_mode": "observations",
        }
    try:
        with sqlite3.connect(resolved) as conn:
            conn.row_factory = sqlite3.Row
            integrity = conn.execute("PRAGMA integrity_check").fetchone()[0]
            if integrity != "ok":
                errors.append(f"integrity_check_failed:{integrity}")
            tables = sorted(
                str(row[0])
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')"
                ).fetchall()
            )
            table_set = set(tables)
            for table in OBSERVATIONS_ALL_TABLES:
                if table not in table_set:
                    errors.append(f"table_missing:{table}")
            for table in sorted(table_set - OBSERVATIONS_ALL_TABLES):
                errors.append(f"unexpected_table:{table}")
            if not errors:
                metadata = _read_metadata(conn)
                if metadata.get("schema_version") != OBSERVATIONS_SCHEMA_VERSION:
                    errors.append("schema_version_mismatch")
                if metadata.get("builder_version") != OBSERVATIONS_BUILDER_VERSION:
                    errors.append("builder_version_mismatch")
                counts = {
                    table: int(conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])
                    for table in OBSERVATIONS_TABLES
                }
                if deep:
                    errors.extend(_deep_referential_errors(conn))
    except sqlite3.Error as exc:
        errors.append(f"sqlite_error:{exc}")
    return {
        "ok": not errors,
        "errors": errors,
        "path": str(resolved),
        "tables": tables,
        "metadata": metadata,
        "counts": counts,
        "schema_version": metadata.get("schema_version"),
        "verification_mode": "observations-deep" if deep else "observations-light",
    }


def _read_metadata(conn: sqlite3.Connection) -> dict[str, Any]:
    rows = conn.execute(f"SELECT key, value_json FROM {OBSERVATIONS_METADATA_TABLE}").fetchall()
    metadata: dict[str, Any] = {}
    for row in rows:
        try:
            metadata[str(row["key"])] = json.loads(str(row["value_json"]))
        except (json.JSONDecodeError, TypeError):
            metadata[str(row["key"])] = row["value_json"]
    return metadata


def _deep_referential_errors(conn: sqlite3.Connection) -> list[str]:
    errors: list[str] = []
    orphan_observations = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM observations AS observation
            LEFT JOIN series_catalog AS series
              ON series.series_key = observation.series_key
            WHERE series.series_key IS NULL
            """
        ).fetchone()[0]
    )
    if orphan_observations:
        errors.append(f"orphan_observations:{orphan_observations}")
    dangling_new_edges = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM observation_revisions AS edge
            LEFT JOIN observations AS new_observation
              ON new_observation.observation_id = edge.new_observation_id
            WHERE new_observation.observation_id IS NULL
            """
        ).fetchone()[0]
    )
    dangling_prior_edges = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM observation_revisions AS edge
            LEFT JOIN observations AS prior_observation
              ON prior_observation.observation_id = edge.prior_observation_id
            WHERE prior_observation.observation_id IS NULL
            """
        ).fetchone()[0]
    )
    if dangling_new_edges or dangling_prior_edges:
        errors.append(f"revision_edge_dangling:{dangling_new_edges + dangling_prior_edges}")
    invalid_revision_kinds = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM observation_revisions
            WHERE revision_kind NOT IN ('revision')
            """
        ).fetchone()[0]
    )
    if invalid_revision_kinds:
        errors.append(f"revision_kind_invalid:{invalid_revision_kinds}")
    orphan_releases = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM release_events AS event
            LEFT JOIN series_catalog AS series
              ON series.series_key = event.series_key
            WHERE series.series_key IS NULL
            """
        ).fetchone()[0]
    )
    if orphan_releases:
        errors.append(f"orphan_release_events:{orphan_releases}")
    orphan_ohlcv = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM ohlcv_observations AS bar
            LEFT JOIN series_catalog AS series
              ON series.series_key = bar.series_key
            LEFT JOIN observations AS observation
              ON observation.observation_id = bar.observation_id
            WHERE series.series_key IS NULL OR observation.observation_id IS NULL
            """
        ).fetchone()[0]
    )
    if orphan_ohlcv:
        errors.append(f"orphan_ohlcv_observations:{orphan_ohlcv}")
    invalid_status = int(
        conn.execute(
            """
            SELECT COUNT(*)
            FROM series_catalog
            WHERE status NOT IN ('available', 'unavailable')
               OR series_key = '' OR canonical_metric = '' OR unit = ''
            """
        ).fetchone()[0]
    )
    if invalid_status:
        errors.append(f"series_catalog_row_invalid:{invalid_status}")
    return errors


def write_observation_metadata(conn: sqlite3.Connection, metadata: Mapping[str, Any]) -> None:
    for key, value in metadata.items():
        conn.execute(
            f"""
            INSERT OR REPLACE INTO {OBSERVATIONS_METADATA_TABLE} (key, value_json)
            VALUES (?, ?)
            """,
            (str(key), json.dumps(value, ensure_ascii=False, sort_keys=True, default=str)),
        )


def iso_utc_observation_now() -> str:
    """Build-time UTC timestamp (same shape as provider provenance stamps)."""

    return datetime.now(UTC).astimezone(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def temporary_sqlite_path(path: Path) -> Path:
    return path.with_name(f".{path.name}.{os.getpid()}.tmp")


def cleanup_sqlite_sidecars(path: Path) -> None:
    for candidate in (
        path,
        path.with_suffix(path.suffix + "-wal"),
        path.with_suffix(path.suffix + "-shm"),
    ):
        try:
            if candidate.exists():
                candidate.unlink()
        except OSError:
            pass


def observations_store_status(path: Path | str) -> dict[str, Any]:
    """Cheap availability probe used by release/serving status checks."""

    resolved = Path(path).expanduser().resolve()
    if not resolved.exists():
        return {"available": False, "path": str(resolved), "reason": "missing"}
    verification = verify_observations_schema(resolved, deep=False)
    return {
        "available": bool(verification.get("ok")),
        "path": str(resolved),
        "reason": None if verification.get("ok") else "verification_failed",
        "verification": verification,
    }


__all__ = [
    "MACRO_SERIES_FORMAT",
    "MARKET_SERIES_FORMAT",
    "MAX_MACRO_POINTS",
    "MAX_MARKET_PERIODS",
    "MAX_RELEASE_EVENTS",
    "OBSERVATIONS_ALL_TABLES",
    "OBSERVATIONS_BUILDER_VERSION",
    "OBSERVATIONS_METADATA_TABLE",
    "OBSERVATIONS_RELATIVE_PATH",
    "OBSERVATIONS_SCHEMA_VERSION",
    "OBSERVATIONS_TABLES",
    "ObservationPoint",
    "ObservationsStore",
    "RELEASE_EVENTS_FORMAT",
    "RevisionEdge",
    "SOURCE_USAGE_RESEARCH_ONLY",
    "VINTAGE_NULL_SENTINEL",
    "cleanup_sqlite_sidecars",
    "create_observations_schema",
    "iso_utc_observation_now",
    "observation_id_for",
    "observation_sort_key",
    "observations_store_status",
    "query_macro_series",
    "query_market_series",
    "query_recent_releases",
    "temporary_sqlite_path",
    "verify_observations_schema",
    "write_observation_metadata",
]
