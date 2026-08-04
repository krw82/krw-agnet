"""Document type mapping, section patterns, and span parameters."""

DOCUMENT_TYPE_DISPLAY = "10-K"
DOCUMENT_TYPE_KEY = "10K"

MAPPING = {
    "10-K": "10K",
    "10-Q": "10Q",
    "earnings_call": "EARNINGS_CALL",
    "research_report": "RESEARCH_REPORT",
    "valuation_model": "VALUATION_MODEL",
}

_REVERSE_MAPPING = {v: k for k, v in MAPPING.items()}


def normalize_doc_type(display: str) -> str:
    """10-K -> 10K, 10-Q -> 10Q, etc."""
    return MAPPING[display]


def denormalize_doc_type(key: str) -> str:
    """10K -> 10-K, 10Q -> 10-Q, etc."""
    return _REVERSE_MAPPING[key]


SECTION_PATTERNS = {
    "cover": r"(?i)^part\s+I\s*$|^cover\s*$|^item\s+1\b(?!\.)",
    "item1": r"(?i)^item\s+1\.\s|^business\b",
    "item1a": r"(?i)^item\s+1A\.\s|^risk\s+factor",
    "item1b": r"(?i)^item\s+1B\.\s|^unresolved\s+staff",
    "item1c": r"(?i)^item\s+1C\.\s|^cybersecurity\b",
    "item2": r"(?i)^item\s+2\.\s|^properties",
    "item7": r"(?i)^item\s+7\.\s|^management's\s+discussion",
    "item7a": r"(?i)^item\s+7A\.\s|^quantitative\s+and\s+qualitative",
    "item8": r"(?i)^item\s+8\.\s|^financial\s+statements",
    "item9a": r"(?i)^item\s+9A\.\s|^controls\s+and\s+procedures",
    "item9b": r"(?i)^item\s+9B\.\s|^other\s+information",
    "item10": r"(?i)^item\s+10\.\s|^directors.*officers|executive\s+compensation",
    "item11": r"(?i)^item\s+11\.\s|^executive\s+compensation",
    "item12": r"(?i)^item\s+12\.\s|^security\s+ownership",
    "item13": r"(?i)^item\s+13\.\s|^certain\s+relationships",
    "item14": r"(?i)^item\s+14\.\s|^exhibits\s+and\s+financial",
    "item15": r"(?i)^item\s+15\.\s|^exhibits\s+and\s+financial\s+statement\s+schedules",
    "item16": r"(?i)^item\s+16\.\s|^exhibits",
}

SPAN_TARGET_CHARS_MIN = 500
SPAN_TARGET_CHARS_MAX = 1200
SPAN_OVERLAP_CHARS = 150
SPAN_HARD_MAX_CHARS = 1500
