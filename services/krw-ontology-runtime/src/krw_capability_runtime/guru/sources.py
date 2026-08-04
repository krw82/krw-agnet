"""Default source catalog for the initial guru-letter ontology scope."""

from __future__ import annotations

from collections.abc import Iterable

from krw_capability_runtime.guru.models import AUTHOR_KEYS, GuruAuthor, GuruSourceDocument


DEFAULT_AUTHORS: tuple[GuruAuthor, ...] = (
    GuruAuthor(
        author_key="buffett",
        display_name="Warren Buffett",
        organization="Berkshire Hathaway",
        corpus_type="shareholder_letters",
        source_types=["shareholder_letter"],
        official_index_url="https://www.berkshirehathaway.com/letters/letters.html",
        notes="Annual shareholder letters and annual report letters.",
    ),
    GuruAuthor(
        author_key="marks",
        display_name="Howard Marks",
        organization="Oaktree Capital Management",
        corpus_type="memos",
        source_types=["memo"],
        official_index_url="https://www.oaktreecapital.com/insights",
        notes="Oaktree memos; treated as investor-letter style source material.",
    ),
    GuruAuthor(
        author_key="ackman",
        display_name="Bill Ackman",
        organization="Pershing Square Holdings",
        corpus_type="shareholder_letters",
        source_types=["shareholder_letter"],
        official_index_url="https://pershingsquareholdings.com/materials?mcat=letters-presentations",
        notes="Shareholder letters in annual and semiannual reports.",
    ),
    GuruAuthor(
        author_key="flatt",
        display_name="Bruce Flatt",
        organization="Brookfield",
        corpus_type="shareholder_letters",
        source_types=["shareholder_letter"],
        official_index_url="https://bam.brookfield.com/reports-sec-filings/letters-shareholders",
        notes="Brookfield shareholder letters.",
    ),
    GuruAuthor(
        author_key="terry_smith",
        display_name="Terry Smith",
        organization="Fundsmith",
        corpus_type="shareholder_letters",
        source_types=["shareholder_letter"],
        official_index_url="https://www.fundsmith.co.uk/documents/",
        notes="Fundsmith annual and semiannual letters.",
    ),
)

DEFAULT_AUTHOR_MAP = {author.author_key: author for author in DEFAULT_AUTHORS}

DEFAULT_SEED_SOURCES: tuple[GuruSourceDocument, ...] = (
    GuruSourceDocument(
        source_id="terry_smith:2025-annual-letter",
        author_key="terry_smith",
        title="Annual Letter to Shareholders 2025",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/4hcfd1pg/2025-fef-annual-letter-web.pdf",
        publication_date="2025",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2025-semiannual-letter",
        author_key="terry_smith",
        title="Semiannual Letter to Shareholders 2025",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/bvgden5v/2025-fef-semi-annual-letter-to-shareholders.pdf",
        publication_date="2025",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2024-annual-letter",
        author_key="terry_smith",
        title="Annual Letter to Shareholders 2024",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/pirmvyly/annual-letter-to-shareholders-2024.pdf",
        publication_date="2024",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2024-semiannual-letter",
        author_key="terry_smith",
        title="Semiannual Letter to Shareholders 2024",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/uznnt5w2/2024-fef-semi-annual-letter-to-shareholders.pdf",
        publication_date="2024",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2023-annual-letter",
        author_key="terry_smith",
        title="Annual Letter to Shareholders 2023",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/31plodnq/2023-fef-annual-letter-to-shareholders.pdf",
        publication_date="2023",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2023-semiannual-letter",
        author_key="terry_smith",
        title="Semiannual Letter to Shareholders 2023",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/cygbfqd0/fef-2023-semi-annual-letter-web.pdf",
        publication_date="2023",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2022-annual-letter",
        author_key="terry_smith",
        title="Annual Letter to Shareholders 2022",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/bm0lyc22/annual-letter-to-shareholders-2022.pdf",
        publication_date="2022",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2022-semiannual-letter",
        author_key="terry_smith",
        title="Semiannual Letter to Shareholders 2022",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/jhnc4xoi/2022-fef-semi-annual-letter.pdf",
        publication_date="2022",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2021-annual-letter",
        author_key="terry_smith",
        title="Annual Letter to Shareholders 2021",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/3wcngjie/2021-fef-annual-letter-to-shareholders-web.pdf",
        publication_date="2021",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
    GuruSourceDocument(
        source_id="terry_smith:2021-semiannual-letter",
        author_key="terry_smith",
        title="Semiannual Letter to Shareholders 2021",
        source_type="shareholder_letter",
        official_url="https://www.fundsmith.co.uk/media/4swb2rkk/semi-annual-letter-to-shareholders-2021.pdf",
        publication_date="2021",
        rights_policy="official_link_only_no_fulltext",
        collection_status="planned",
    ),
)


def parse_author_keys(raw: str | Iterable[str] | None) -> list[str]:
    """Parse comma-separated or repeated author keys into canonical keys."""
    if raw is None:
        return list(AUTHOR_KEYS)
    parts: list[str] = []
    if isinstance(raw, str):
        candidates = raw.split(",")
    else:
        candidates = []
        for item in raw:
            candidates.extend(str(item).split(","))
    for candidate in candidates:
        normalized = candidate.strip().lower().replace("-", "_")
        if normalized:
            parts.append(normalized)
    return parts or list(AUTHOR_KEYS)


def selected_authors(author_keys: Iterable[str] | None = None) -> list[GuruAuthor]:
    """Return default authors filtered by author key, preserving catalog order."""
    selected_keys = list(author_keys) if author_keys is not None else list(AUTHOR_KEYS)
    unknown = sorted(set(selected_keys) - set(DEFAULT_AUTHOR_MAP))
    if unknown:
        raise ValueError(f"Unknown guru author key(s): {', '.join(unknown)}")
    requested = set(selected_keys)
    return [author for author in DEFAULT_AUTHORS if author.author_key in requested]


def selected_seed_sources(author_keys: Iterable[str] | None = None) -> list[GuruSourceDocument]:
    """Return official direct-document seeds for selected authors."""
    selected_keys = set(author_keys) if author_keys is not None else set(AUTHOR_KEYS)
    return [source for source in DEFAULT_SEED_SOURCES if source.author_key in selected_keys]
