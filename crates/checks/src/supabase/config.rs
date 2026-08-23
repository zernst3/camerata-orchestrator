//! `supabase/config.toml` `[api].schemas` parse — the exposed-schema set every RLS finding
//! in this module scopes against (see the design memo §3, step 1).

use std::collections::BTreeSet;

/// Parse the `[api].schemas` list from a `supabase/config.toml`'s content, returning the set
/// of API-exposed schema names.
///
/// **Default divergence, documented deliberately:** falls back to the single-schema default
/// `{"public"}` per this executor build's spec (memo §3: "parse config.toml `[api].schemas`
/// (default `{"public"}`)"). This is narrower than Supabase's OWN CLI default of
/// `["public", "storage", "graphql_public"]`, which `SUPABASE-EXPOSURE-SCHEMAS-1`'s corpus
/// entry documents as the real-world default. The narrower default is a deliberate
/// simplification carried over from the build spec, not a correction of the corpus rule —
/// `storage`/`graphql_public` tables are covered by the dedicated storage-policy rules, not
/// this RLS checker, so scoping this checker's default to `public` alone avoids it silently
/// producing findings for schemas the storage-specific rules already own. Revisit if a future
/// pass wants this checker to also read `SUPABASE-EXPOSURE-SCHEMAS-1`'s parse directly.
///
/// Never panics: malformed TOML, a missing `[api]` table, or an empty/absent `schemas` array
/// all fall back to the default set.
pub fn parse_exposed_schemas(config_toml: &str) -> BTreeSet<String> {
    let value: toml::Value = match config_toml.parse() {
        Ok(v) => v,
        Err(_) => return default_schemas(),
    };
    let schemas = value.get("api").and_then(|api| api.get("schemas")).and_then(|s| s.as_array());
    match schemas {
        Some(arr) => {
            let set: BTreeSet<String> = arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            if set.is_empty() {
                default_schemas()
            } else {
                set
            }
        }
        None => default_schemas(),
    }
}

fn default_schemas() -> BTreeSet<String> {
    ["public".to_string()].into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_falls_back_to_public_only() {
        let got = parse_exposed_schemas("");
        assert_eq!(got, default_schemas());
    }

    #[test]
    fn malformed_toml_falls_back_to_default_without_panicking() {
        let got = parse_exposed_schemas("not valid [[[ toml");
        assert_eq!(got, default_schemas());
    }

    #[test]
    fn parses_single_schema() {
        let toml = "[api]\nschemas = [\"public\"]\n";
        assert_eq!(parse_exposed_schemas(toml), default_schemas());
    }

    #[test]
    fn parses_multiple_schemas() {
        let toml = "[api]\nschemas = [\"public\", \"app\", \"billing\"]\n";
        let got = parse_exposed_schemas(toml);
        let expected: BTreeSet<String> = ["public", "app", "billing"].into_iter().map(String::from).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn missing_api_table_falls_back_to_default() {
        let toml = "[db]\nport = 5432\n";
        assert_eq!(parse_exposed_schemas(toml), default_schemas());
    }

    #[test]
    fn empty_schemas_array_falls_back_to_default() {
        let toml = "[api]\nschemas = []\n";
        assert_eq!(parse_exposed_schemas(toml), default_schemas());
    }
}
