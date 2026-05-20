#!/usr/bin/env bash
# Enforce the facade-integrity rule from docs/code_organization_policy.md:
# directory mod.rs files declare child modules with plain `mod`, except for
# an allowlist of children named from outside the library crate.
set -euo pipefail

# Allowlist: "<path-under-src> <child>" pairs. Children named directly by a
# monodex::... path in tests/ or main.rs. Revisit when the crate API surface
# is designed deliberately; see code_organization_policy.md.
allow=$(cat <<'EOF'
app/mod.rs commands
app/mod.rs config
app/commands/mod.rs crawl
app/commands/mod.rs init_db
app/commands/mod.rs purge
app/commands/mod.rs search
engine/mod.rs fts
engine/mod.rs identifier
engine/mod.rs identity
engine/mod.rs retrieval
engine/mod.rs schema
engine/mod.rs storage
EOF
)

violations=""
while IFS= read -r -d '' modfile; do
    rel=${modfile#src/}
    while IFS= read -r line; do
        # Extract the child name from the leading "pub[(...)] mod NAME".
        # Anchored at the start so a later "mod" in a comment cannot mislead it.
        decl=$(printf '%s\n' "$line" | sed -E 's/^[0-9]+:[[:space:]]*pub(\([^)]*\))?[[:space:]]+mod[[:space:]]+([A-Za-z_][A-Za-z0-9_]*).*/\2/')
        if ! printf '%s\n' "$allow" | grep -qxF "$rel $decl"; then
            violations+="$modfile: $line"$'\n'
        fi
    done < <(grep -nE '^[[:space:]]*pub(\([^)]*\))?[[:space:]]+mod[[:space:]]' "$modfile" || true)
done < <(find src -name mod.rs -print0)

if [[ -n "$violations" ]]; then
    echo "Facade violation: directory mod.rs files must declare child modules with"
    echo "plain 'mod', not 'pub mod' or 'pub(...) mod', unless allowlisted in this"
    echo "script. See the Facade integrity section of docs/code_organization_policy.md."
    echo
    printf '%s' "$violations"
    echo
    echo "Fix by re-exporting the needed item from the directory mod.rs, or by"
    echo "changing the caller to a path the facade already exposes. Add to the"
    echo "allowlist only when a caller must name the child module itself."
    exit 1
fi
