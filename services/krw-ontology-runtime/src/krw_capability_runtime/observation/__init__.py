"""Observation data layer: provider ports, seed catalog, and served store.

Observation series (prices, valuation multiples, macro indicators) are
advisory-only research context. They never become filing evidence, never
support strong claims, and never ground recommendations or price targets.
Vendor names are internal collection plumbing; the answer layer scrubs them.

Capability-runtime note: this serving copy imports the ports, the seed
catalog, and the read-side store only. The upstream builder and provider
adapters (``observation/builder.py``, ``observation/providers/*``) are
build-time collection plumbing: the serving runtime never touches a network,
and the immutable release carries the already-built ``observations.sqlite``.
"""

from krw_capability_runtime.observation.ports import (
    ObservationProvider,
    RawObservation,
    SeriesFetchRequest,
    SeriesFetchResult,
)
from krw_capability_runtime.observation.seed import (
    KNOWN_FACTOR_LABELS,
    SeriesDefinition,
    load_series_seed,
)
from krw_capability_runtime.observation.store import (
    OBSERVATIONS_BUILDER_VERSION,
    OBSERVATIONS_SCHEMA_VERSION,
    ObservationsStore,
    verify_observations_schema,
)

__all__ = [
    "KNOWN_FACTOR_LABELS",
    "OBSERVATIONS_BUILDER_VERSION",
    "OBSERVATIONS_SCHEMA_VERSION",
    "ObservationProvider",
    "ObservationPoint",
    "ObservationsStore",
    "RawObservation",
    "SeriesDefinition",
    "SeriesFetchRequest",
    "SeriesFetchResult",
    "load_series_seed",
    "verify_observations_schema",
]
