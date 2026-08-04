"""Online read-runtime validator namespace.

Validation modules used by the offline ontology builder are intentionally not
imported here. Read handlers import the one validator they need directly,
which prevents a package import from pulling the build pipeline into a shared
capability process.
"""
