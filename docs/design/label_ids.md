# Identifier and reference syntax

This document defines the syntax of identifiers, locators, and reports used by Monodex at the CLI and in storage. Validation and composition for the rules below live in `src/engine/identifier.rs`.

## Terminology

- **Catalog** — a Monodex-assigned name for a data source. Few and stable. Chosen by the user.
- **Label** — a Monodex-assigned name for a version or snapshot of a catalog (branch, commit, tag, working-directory state, time snapshot). Many and diverse.
- **Path** — a location within a label. The identity of a path is determined by the underlying data source. Monodex does not assign or constrain path syntax.
- **Locator** — a structured string that identifies content within a grammar. Two locator grammars exist: **breadcrumbs** (catalog-relative, of the form `package:file:symbol`) and **references** (globally qualified, of the form `@catalog:label:path` and its sub-forms). Locators are parseable; their structural characters must be encoded when they appear inside path or identifier data.
- **Report** — human-facing output (CLI stdout, error messages, log lines). Reports are not locators. They use decoded paths and separate fields by visual devices that cannot collide with identifier characters.

Catalogs and labels are identifiers Monodex owns. Paths are external data Monodex indexes. This distinction is load-bearing: it determines what Monodex may constrain (catalogs and labels) versus what it must represent faithfully (paths).

## What is parsed today

Currently the CLI accepts only the bare forms:

- `--catalog <catalog>` — a single bare catalog name.
- `--label <label>` — a single bare label name.

Composite forms (`label:path`, `kind=payload`, `@catalog:label`, `@catalog:label:path`, etc.) are reserved grammar but are not parsed yet. The reserved characters described below are rejected at validation time so they remain available when the composite forms land.

## Catalog syntax (kebab-case)

```
^[a-z0-9]+(?:-[a-z0-9]+)*$
```

- Length 1–64 characters.
- Lowercase ASCII alphanumeric words separated by single `-`.
- No leading, trailing, or consecutive `-`.

Examples:

- Valid: `my-repo`, `frontend`, `backend-api`
- Invalid: `My-Repo` (uppercase), `left--right` (consecutive `-`), `trailing-` (trailing separator)

## Label syntax (Git-like)

```
^[a-z0-9]+(?:[./=-][a-z0-9]+)*$
```

- Length 1–128 characters.
- Lowercase ASCII alphanumeric words separated by single `.`, `/`, `-`, or `=`.
- No leading, trailing, or consecutive separators.
- `=` is a permitted separator character but is not interpreted as a typed-form delimiter today (see §Typed labels below).

Examples:

- Valid: `main`, `feature/x`, `release/v1.2.3`, `working-dir`, `branch=main`, `repo/sub/feature`
- Invalid: `feature_login` (underscore), `EXAMPLE` (uppercase), `first//second` (consecutive separators)

## Forbidden characters

Forbidden in bare catalog and label identifiers:

```
:  @  +  #  whitespace  control characters
```

These are reserved for current or future locator grammar. `+` and `#` are not used by Monodex's grammar today but are reserved to keep future extensions non-breaking. The rationale for reserving them specifically (rather than some other unused punctuation) is approximately: `#` is a comment character in most shells and the fragment delimiter in URLs, so a label containing `#` would be awkward to pass on the command line and would conflict with URL-shaped projections of references; `+` was historically the space encoding in URL query strings and similarly creates ambiguity if used in a label that ever appears in a URL-shaped context. Reserving them now keeps the door open without forcing a decision today.

`=` is additionally forbidden in catalogs. `=` is permitted in labels but not interpreted; a label containing `=` is an opaque identifier.

## Storage form (`label_id`)

Internally Monodex uses the qualified form `<catalog>:<label>` as the storage key for label rows and the value stored in `active_label_ids` on chunks. For example, `rushstack:main` or `frontend:feature/login-flow`.

This qualified form appears only in:

- The `label_id` field of `label_metadata` rows (primary key).
- The `active_label_ids` list on `chunks` rows.
- Internal log output and debug strings.

Users never type or see the qualified form directly. The `LabelId` type in `src/engine/identifier.rs` is the only place that constructs it; downstream code receives an already-validated `LabelId` rather than a raw string.

## Paths

### Principle

Paths are facts about external systems. Monodex does not assign path syntax and must not refuse to index a file because its path contains a character that collides with Monodex's locator grammars.

This rules out two failure modes:

- **Rejection** — refusing to crawl a file because its path contains `:`, `@`, or `=`. Monodex does not control what filenames appear in a Git tree.
- **Silent omission** — skipping such files with a warning. The user gets a crawl reported as "successful" but search results are missing content they expect. Worse than rejection because the failure is invisible.

Both are forbidden.

### Storage

Paths are stored verbatim in the `relative_path` column. No normalization, rewriting, or character substitution. The path round-trips bit-for-bit with what the data source reported.

### Encoding at locator boundaries

When a path appears inside a locator — a breadcrumb or a reference, any context where it is concatenated with grammar characters — it is **percent-encoded per RFC 3986**.

Characters that must be encoded in a path within a locator:

- Grammar-reserved: `:`, `@`, `=`, `+`, `#`
- The escape character itself: `%`
- Whitespace and control characters

`/` is not encoded. It is a legitimate path separator and does not collide with any locator-grammar character.

Decoding is the inverse: percent-sequences in the path segment of a locator are decoded before lookup. Storage still holds the decoded form.

Percent-encoding was chosen over backslash or quote-based escaping because it survives shells, JSON, and YAML without re-escaping, and it keeps ordinary paths mostly readable.

The encode/decode helpers live in `src/engine/breadcrumb.rs`.

### Examples

| Stored path                                   | In a breadcrumb                                          | In a global reference                                         |
| --------------------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------- |
| `libraries/node-core-library/src/JsonFile.ts` | `@rushstack/node-core-library:JsonFile.ts:JsonFile.load` | `@rushstack:main:libraries/node-core-library/src/JsonFile.ts` |
| `src/index.ts`                                | `node-core-library:index.ts:example`                    | `@my-repo:main:src/index.ts`                                  |
| `src/weird:file.ts`                           | `node-core-library:weird%3Afile.ts:example`             | `@my-repo:main:src/weird%3Afile.ts`                           |
| `50%off/notes.md`                             | `node-core-library:50%25off/notes.md:example`           | `@my-repo:main:50%25off/notes.md`                             |

## Breadcrumbs

Breadcrumbs have the form `package:file:symbol`. The `:` is a structural separator within the breadcrumb grammar. Path components embedded in a breadcrumb are percent-encoded per the rules above. Example: a file named `weird:file.ts` in package `node-core-library` renders as `node-core-library:weird%3Afile.ts:JsonFile.load`.

Breadcrumbs are catalog-relative — they do not begin with `@catalog`. A reader who needs the fully-qualified identity of a breadcrumb must consult the surrounding context (the search result's `catalog` field, the crawl's scope, etc.).

## Reports (human-facing output)

Report lines in CLI output (e.g. `Source:` lines, error messages, progress output) are not locators and must not look like them. Paths in reports use the stored (decoded) form. Fields within a report line are separated by a visual device that cannot appear in a catalog name — parentheses, `/`, or a newline — never by `:` or `@`.

Correct:

```
Source: (my-repo) src/weird:file.ts
Source: my-repo / src/weird:file.ts
```

Incorrect (looks like a locator, isn't one):

```
Source: my-repo:src/weird:file.ts
Source: @my-repo:src/weird:file.ts
```

## Reserved grammar: typed labels

The grammar below is not implemented and is not on the roadmap. It exists in this document because reserving the syntactic space ahead of time is what allowed the current implementation to make confident choices about which characters are safe in bare labels. If a use case arises that benefits from typed labels, this design is ready; if no such use case arises, the design lies dormant and costs nothing.

The form `kind=payload` is reserved for a possible future typed-label grammar. Examples of intended kinds: `branch=main`, `commit=abc123`, `tag=v1.2.3`, `local=working-dir`, `snapshot=2026-04-16T12-00`.

`kind` would be a reserved identifier from a small fixed vocabulary; `payload` would be opaque and may contain `/`. The typed form eliminates ambiguity with user-created branch names that coincide with reserved kinds (a user with a branch literally named `commit` could disambiguate via `branch=commit`).

In the current implementation, `=` is a permitted character in label identifiers but is not parsed or interpreted: `--label branch=main` today yields a label literally named `branch=main`. Users may adopt `kind=payload` as a naming convention in their own automation, and such names will remain valid if the typed form is parsed natively in the future.

The `kind` grammar would be:

```
^[a-z0-9]+$
```

## Reserved grammar: composite references

Same reservation discipline as typed labels above: this grammar is designed but not implemented or scheduled. It exists to validate that the characters reserved in bare identifiers are sufficient to support a useful future locator grammar without breaking changes.

The full set of forms a future parser would accept:

```
path
label
catalog
label:path
kind=payload:path
@catalog:path
@catalog:label
@catalog:kind=payload
@catalog:label:path
@catalog:kind=payload:path
```

Parsing rules:

1. If the string starts with `@`, parse catalog first: `@catalog:...`.
2. Split the remaining string on `:` (left-to-right, respecting path encoding):
   - 1 segment → label or path (based on context)
   - 2 segments → `label:path` or `kind=payload:path`
   - 3 segments → `@catalog:label:path` or `@catalog:kind=payload:path`
3. Within a label segment: if `=` is present, parse as `kind=payload`; otherwise treat as opaque.
4. `/` is never parsed structurally; it is always part of a label or a decoded path.
5. Path segments are percent-decoded before use.

None of this composite parsing is implemented today. Reserved characters are rejected at validation time so the grammar remains available.
