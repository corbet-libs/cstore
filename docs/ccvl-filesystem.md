# CCVL filesystem contract

cstore must support an existing CCVL tree without flattening it into resolved
application JSON. This map was checked against CCVL source revision
`9d0612fcaf4526d94a88f6397b79edcb9ff657f9`: `ccvl.json`, `.agent/AGENT.md`,
`.agent/docs/{architecture,data-model,private-downstream}.md`, the three product
directory READMEs, and `.agent/src/{workspace,opportunity,content,application}.rs`.

## Existing and intended structure

```text
ccvl.json                                      workspace manifest, schema 8
interview/                                     private working knowledge
  imports/                                     original source documents
  profile.md                                   evidence and claim ledger
  journal.md                                   progress, conflicts, preferences
  stations.toml                                allocation of verified facts
cvl/                                           approved general documents
  profile.toml                                 approved render identity
  assets/                                      profile assets
  shared/<family>/                             family-owned reusable settings
  <cv|cl>/<style>/
    style.toml, contract.toml, scaffold.toml
    content/<language>/<country>/wording.toml
    <substyle>/substyle.toml
    <substyle>/<language>/<country>/
      content.toml, strings.toml, layout.toml
      typst/, pdf/, preview/
opportunities/<organisation>/<position>/
  application.toml                             canonical tailored record
  posting.md                                   required role/source reference
  research.md                                  optional attributable research
  interview-<stage>.md, submission.md, outcome.md
  typst/, pdfs/                                retained generated outputs
.agent/                                        implementation and tooling
  scaffolds/, schemas/, skills/, typst/, src/, core/, docs/
```

The public upstream intentionally contains only neutral interview/opportunity
scaffolding alongside its separately licensed personal document showcase. Real
private downstreams fill the same data domains. Missing optional scaffolds are
not missing data to synthesize. Neutral library tests must never copy the showcase
author's facts or personal assets.

## Identities and references

An opportunity's two directory keys are its product identity. The constructor
sets `job.id` to `<organisation>--<position>`; the application record also has
`schema_version = 4` and `revision`. Do not replace these with generated UUIDs.
A product rename must update these related fields through the product operation.

General document leaves may reference
`../../../content/<language>/<country>/wording.toml`. References stay inside the
same document, style and locale. Tables merge recursively; scalar and array
overrides replace their complete values. Opportunity records are self-contained
and may not use these shared-wording references. Preserve the authored source,
reference and override separately, including comments and unknown bytes.

Style internals are independent. Harvard may use `src/`; other families use
`layout.typ` or `parts/`. Storage must not assume one renderer path, page count,
font, paragraph structure or country-to-paper mapping.

## Consequences for cstore

- Map existing paths to portable record keys through the consumer's file adapter.
  Keep original bytes, relative paths, directories and relevant file metadata.
- Database conversion must retain the complete authored representation alongside
  any product read models. Resolved render input is insufficient for restoration.
- Restore the same tree so ordinary CCVL discovery, relative references and build
  entry points work. `ccvl.json` identifies the root; source reads are bounded by it.
- Include imported evidence, style/font dependencies and retained outputs in the
  selected workspace scope. A file being untracked or ignored does not authorize
  omitting it from a backup. Keep mechanism and private-content ownership explicit.
- Keep cstore's receipts and recovery metadata separate from user payloads, with
  an explicitly configured control location. They must not require new product
  data roots or overwrite CCVL's existing revision/header conventions.
- Current CCVL readers parse TOML into JSON values for validation/rendering. That
  conversion is lossy for comments; cstore captures source bytes before it.

Acceptance needs a neutral tree with shared wording and a leaf override, a
self-contained opportunity plus posting, imported evidence and binary outputs.
After a database round trip every path and source byte must be retained. Product
validation/rendering remains a separate integration gate.

