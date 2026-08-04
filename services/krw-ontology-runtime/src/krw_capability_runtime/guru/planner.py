"""Planning helpers for guru-letter ontology collection."""

from __future__ import annotations

from pathlib import Path

from krw_capability_runtime.guru.models import (
    GuruCollectionPlan,
    GuruOntologyManifest,
    GuruPipelineStage,
    GuruSourceDocument,
    GuruSourceManifest,
    utc_now_iso,
)
from krw_capability_runtime.guru.sources import selected_authors, selected_seed_sources


GURU_OBJECT_TYPES = [
    "GuruAuthor",
    "GuruSourceDocument",
    "GuruSourceSpan",
    "GuruEvidenceExcerpt",
    "GuruConcept",
    "GuruPrinciple",
    "GuruQuestion",
    "GuruSearchTarget",
    "GuruApplicationRule",
    "GuruRelationship",
    "GuruSafetyRule",
    "GuruIdea",
    "GuruHeuristic",
    "GuruAntiPattern",
    "GuruDecisionCriterion",
    "GuruRiskFrame",
    "GuruValuationFrame",
    "GuruTimeHorizonFrame",
    "GuruBehavioralWarning",
    "GuruInvestorIntent",
    "GuruQuestionTemplate",
    "GuruQuestionRoute",
    "GuruAnswerPlaybook",
    "GuruAnswerSection",
    "GuruDataNeed",
    "GuruClarifyingQuestion",
]

EXISTING_ONTOLOGY_MAPPING = [
    {
        "existing": "SourceDocument",
        "guru": "GuruSourceDocument",
        "boundary": "separate schema; no objects.yaml change",
    },
    {
        "existing": "SourceSpan",
        "guru": "GuruSourceSpan",
        "boundary": "private cache only for full text spans",
    },
    {
        "existing": "EvidenceQuote",
        "guru": "GuruEvidenceExcerpt",
        "boundary": "short excerpts only; no full-text public storage",
    },
    {
        "existing": "ResearchClaim",
        "guru": "GuruConcept",
        "boundary": "thinking-pattern candidate, not company claim",
    },
    {
        "existing": "Edge",
        "guru": "GuruRelationship",
        "boundary": "lens relationship, not company evidence graph edge",
    },
]


def build_planned_source_documents(author_keys: list[str] | None = None) -> list[GuruSourceDocument]:
    """Create one official-index source document per selected author.

    This is intentionally a seed manifest, not network discovery. Actual document
    collection will be a later phase.
    """
    documents: list[GuruSourceDocument] = []
    for author in selected_authors(author_keys):
        documents.append(
            GuruSourceDocument(
                source_id=f"{author.author_key}:official_index",
                author_key=author.author_key,
                title=f"{author.display_name} official source index",
                source_type=author.source_types[0],
                official_url=author.official_index_url,
                rights_policy=author.default_rights_policy,
                collection_status="planned",
            )
        )
    return documents


def build_source_manifest(author_keys: list[str] | None = None) -> GuruSourceManifest:
    """Build a source manifest without fetching remote documents."""
    authors = selected_authors(author_keys)
    selected_keys = [author.author_key for author in authors]
    return GuruSourceManifest(
        generated_at=utc_now_iso(),
        authors=authors,
        planned_sources=build_planned_source_documents(selected_keys),
        seed_sources=selected_seed_sources(selected_keys),
        source_policy={
            "full_text_public_storage": False,
            "raw_collection_started": False,
            "official_sources_only": True,
            "excerpt_word_limit": 25,
        },
    )


def build_ontology_manifest(author_keys: list[str] | None = None) -> GuruOntologyManifest:
    """Build the empty ontology artifact manifest for a planned workspace."""
    authors = selected_authors(author_keys)
    return GuruOntologyManifest(
        generated_at=utc_now_iso(),
        author_keys=[author.author_key for author in authors],
        object_types=GURU_OBJECT_TYPES,
        notes=[
            "MCP and skills integration are intentionally out of scope.",
            "Existing KRW company ontology schema is not extended.",
            "Guru lenses support standalone questions without a ticker.",
            "Consultation objects route investor intents to guru lenses and future data needs.",
        ],
    )


def build_collection_plan(
    root: Path,
    author_keys: list[str] | None = None,
    *,
    running_root: Path | None = None,
) -> GuruCollectionPlan:
    """Build the planned guru collection DAG without starting collection."""
    authors = selected_authors(author_keys)
    selected_keys = [author.author_key for author in authors]
    running_path = running_root or (root.parent / f"{root.name}-running")
    return GuruCollectionPlan(
        generated_at=utc_now_iso(),
        author_keys=selected_keys,
        workspace={
            "root": str(root),
            "running_root": str(running_path),
            "source_manifest": str(root / "source_manifest.yaml"),
            "collection_plan": str(root / "collection_plan.json"),
            "ontology_manifest": str(root / "ontology_manifest.json"),
            "reviewed_dir": str(root / "reviewed"),
            "published_dir": str(root / "published"),
            "raw_dir": str(running_path / "raw"),
            "parsed_dir": str(running_path / "parsed"),
            "spans_dir": str(running_path / "spans"),
            "generated_dir": str(running_path / "generated"),
        },
        stages=[
            GuruPipelineStage(
                stage="source_manifest",
                status="planned",
                writes_full_text=False,
                notes="Write official source index metadata only.",
            ),
            GuruPipelineStage(
                stage="source_discovery",
                status="pending",
                writes_full_text=False,
                notes="Fetch official index pages and discover PDF/HTML letter links.",
            ),
            GuruPipelineStage(
                stage="raw_fetch",
                status="pending",
                writes_full_text=True,
                notes="Explicit guru fetch phase; writes private raw files only.",
            ),
            GuruPipelineStage(
                stage="parse_spans",
                status="pending",
                writes_full_text=True,
                notes="Explicit guru parse phase; writes private parsed text and spans.",
            ),
            GuruPipelineStage(
                stage="agent_sdk_concept_induction",
                status="pending",
                writes_full_text=False,
                notes=(
                    "Explicit guru extract phase for lens and consultation objects; "
                    "dry-run unless --execute-agent-sdk is passed."
                ),
            ),
        ],
        ontology_object_types=GURU_OBJECT_TYPES,
        existing_ontology_mapping=EXISTING_ONTOLOGY_MAPPING,
        next_commands=[
            "krw-ontology guru fetch --root <root> --running-root <running-root>",
            "krw-ontology guru parse --root <root> --running-root <running-root>",
            "krw-ontology guru extract --root <root> --running-root <running-root>",
            "krw-ontology guru verify --root <root> --running-root <running-root>",
        ],
    )
