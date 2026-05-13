# Sample Documentation

This document demonstrates various markdown features for testing.

## Installation

To install the tool, run the following command:

```bash
cargo install example-tool
```

Make sure you have Rust 1.70 or later installed.

## Configuration

Create a configuration file at `~/.config/example/config.json`:

```json
{
  "projects": {
    "my-project": {
      "type": "library",
      "path": "/path/to/my-project"
    }
  }
}
```

## Usage

### Basic Commands

To run the tool:

```bash
example-tool init
example-tool run --project my-project
```

This will:
1. Scan all source files
2. Process the inputs
3. Generate the output

### Advanced Options

To customize behavior:

```bash
example-tool run --verbose
```

## Troubleshooting

**Q: I get "No config found" error**

A: Make sure you have created the config file at `~/.config/example/config.json`.

**Q: I get "No database" error**

A: Run `example-tool init` first to initialize.
