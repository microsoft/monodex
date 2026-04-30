# Editing the JSON Schemas

The `.schema.json` files in this directory are JSON Schemas for Monodex's user-editable config files. They serve two purposes: editor integration (autocomplete, validation, inline documentation via `$schema` URLs) and as a published release artifact hosted on a Microsoft-managed schema server.

These files must remain strict, commentless JSON. Do not add comments, even in JSON-with-comments form. Do not reference repo-internal paths inside the schemas. Editors may consume them via `$schema` URL fetch and may not tolerate non-standard JSON.

The shapes defined here are mirrored by Rust structs that perform runtime validation:

| JSON Schema           | Rust struct file             |
| --------------------- | ---------------------------- |
| `config.schema.json`  | `src/app/config.rs`          |
| `context.schema.json` | `src/app/context.rs`         |
| `crawl.schema.json`   | `src/engine/crawl_config.rs` |

When adding or renaming a field, update both the JSON Schema here and the corresponding Rust struct in the same change. See [docs/design/monodex_files.md](../docs/design/monodex_files.md) for the full validation model and rationale.
