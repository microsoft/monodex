# API Documentation

This document describes the core APIs for the monodex project.

## Installation

To install monodex, run the following command:

```bash
cargo install monodex
```

Make sure you have Rust 1.70 or later installed.

## Configuration

Create a configuration file at `~/.monodex/config.json`:

```json
{
  "catalogs": {
    "node-core-library": {
      "type": "monorepo",
      "path": "/path/to/rushstack/libraries/node-core-library"
    }
  }
}
```

## Usage

### Indexing

To index a catalog:

```bash
monodex init-db
monodex crawl --catalog node-core-library --label main --commit HEAD
```

This will:
1. Scan all TypeScript files
2. Generate embeddings
3. Store in the local LanceDB database

### Querying

To search the index:

```bash
monodex search "JsonFile"
```

## Troubleshooting

**Q: I get "No config found" error**

A: Make sure you have created the config file at `~/.monodex/config.json`.

**Q: I get "No monodex database" error**

A: Run `monodex init-db` first to create the database.
