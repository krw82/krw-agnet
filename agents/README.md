# Direct-authored agent programs

These packages are the canonical KRW agent source. They are edited directly;
there is no SKILL/plugin importer and no per-run conversion step.

| Package | Entrypoints |
|---|---|
| `krw-ontology` | Korean company research, earnings deep dive, scenario sensitivity, idea generation |
| `krw-ontology-en` | English company research |
| `krw-feed` | news discovery, selected-news research, market-move research, news/filing correlation |
| `krw-router` | classification-only durable-run routing |
| `krw-notebook` | merge/reorganize notebook and extract open questions |
| `krw-display` | optional display-plan creation from existing answer units |
| `krw-source-filing` | one immutable verified filing follow-up |
| `krw-guru-advisor` | fixed Ackman, Buffett, Flatt, Marks, and Terry Smith entrypoints |

Each package pins only the canonical contracts used by its own capabilities.
Ontology-only packages pin nine contracts, `krw-feed` pins seventeen (the base
ontology contracts plus public feed and catalog-search contracts), and
`krw-source-filing` pins eighteen (public filing contracts plus normalized
result, typed state, and AnswerIR contracts). `krw-guru-advisor` pins the nine shared contracts plus
all fourteen source-pinned default Guru workflow contracts. Its three Guru MCP
capabilities and three filing capabilities are separate closed mappings. Every
capability result contract set includes
`normalized-capability-result/v1`. Personal filing contracts and tools are not
declared. Every model-driven state has an explicit `role_id`; deterministic
states have none. Every terminal state has an explicit terminal disposition.
Prompts contain only bounded planning, analysis, and rendering guidance.
Workflow order, capability allowlists, action limits, and available hard
checks live in typed AgentSpec fields and validator programs.

The Guru package exposes only two narrow model-authored capability inputs after
retrieval: one investigation-question draft and one evidence analysis. The
kernel's closed argument assemblers construct the physical brief and review
calls from hash-bound committed artifacts, so a model cannot echo, replace, or omit the private ResearchPack,
fixed identity, sealed brief, or filing context. Its company-evidence role also
declares a closed child ABI: one child, depth one, a reservation inherited from
the parent, and only the three read-only ontology capabilities. AgentSpec
validation bounds this generically and capability-catalog startup binds the
exact Guru role/capability set. The run engine durably reserves, invokes, and
completes that child without inheriting or consuming the parent transcript.

Run `agents/check-all.sh` to check the source catalog, security invariants,
every spec, and build/load every immutable image. Read `SEMANTIC_GAPS.md`
before enabling a package whose capability mapping is not live in the runtime.
