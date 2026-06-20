# 2026-06-20: Ruby/Rails Rules Corpus

## Status
COMPLETE: 11 rule files created and loaded; all tests passing; build green.

## Decision
Add a comprehensive Ruby/Rails rule corpus under `crates/rules/principles/ruby/`, mirroring the format and rigor of existing language corpora (Python, Java, C#, Go, JavaScript/Express).

## Rationale
The Ruby language and Rails framework are widely used for backend and full-stack systems, and Camerata's rule governance needs to cover this domain explicitly. The corpus provides enforceable guidance on security, architectural patterns, and idioms for teams building Ruby/Rails applications.

## Rules Added

### Security & Query Parameterization
1. **RUBY-RAILS-PARAMETERIZED-QUERIES-1** (`ruby-rails-parameterized-queries-1.toml`)
   - Domain: `ruby:rails`
   - Enforcement: Mechanical
   - Requires SQL queries to use parameterized binding via ActiveRecord placeholders (?, :named) instead of string interpolation
   - Enforcement via Brakeman + RuboCop in CI

2. **RUBY-RAILS-STRONG-PARAMS-1** (`ruby-rails-strong-params-1.toml`)
   - Domain: `ruby:rails`
   - Enforcement: Mechanical
   - Requires controllers to use Rails strong_parameters to whitelist request inputs
   - Default option: use `permit()` to explicitly allow parameters
   - Prevents mass-assignment vulnerabilities

3. **RUBY-NO-HARDCODED-SECRETS-1** (`ruby-no-hardcoded-secrets-1.toml`)
   - Domain: `ruby`
   - Enforcement: Mechanical
   - Forbids hardcoded API keys, database passwords, OAuth tokens, and private keys
   - Default: Load from ENV or Rails encrypted credentials
   - Enforcement via Brakeman, detect-secrets, GitGuardian in CI

4. **RUBY-AVOID-EVAL-SEND-1** (`ruby-avoid-eval-send-1.toml`)
   - Domain: `ruby`
   - Enforcement: Mechanical
   - Forbids `eval()`, `instance_eval()`, `class_eval()`, and dynamic `send()` with user input
   - Default: Use dispatch tables (hash or case statement) with fixed allowlists
   - Prevents arbitrary code execution vulnerabilities

### Architecture & Design Patterns
5. **RUBY-THIN-CONTROLLERS-1** (`ruby-thin-controllers-1.toml`)
   - Domain: `ruby:rails`
   - Enforcement: Structured
   - Requires controllers to stay thin; business logic lives in models, services, or repositories
   - Aligns with the "fat models, skinny controllers" Rails idiom
   - Makes logic testable, reusable, and separately changeable from routing

6. **RUBY-ACTIVERECORD-SCOPES-1** (`ruby-activerecord-scopes-1.toml`)
   - Domain: `ruby:rails`
   - Enforcement: Structured
   - Requires common query patterns to be encapsulated in ActiveRecord scopes
   - Centralizes query definitions to prevent drift and duplication
   - Improves readability and composition (e.g., `User.active.recent.with_orders`)

7. **RUBY-EAGER-LOAD-ASSOCIATIONS-1** (`ruby-eager-load-associations-1.toml`)
   - Domain: `ruby:rails`
   - Enforcement: Mechanical
   - Requires eager loading of associations with `includes()` or `eager_load()` to prevent N+1 queries
   - Enforced by Bullet gem or custom N+1 detection in CI
   - Critical for performance at scale

### Code Idioms & Quality
8. **RUBY-FROZEN-STRING-LITERAL-1** (`ruby-frozen-string-literal-1.toml`)
   - Domain: `ruby`
   - Enforcement: Mechanical
   - Requires all .rb files to declare `# frozen_string_literal: true`
   - Prevents accidental string mutations across references
   - Enforced by RuboCop; standard in Rails 6+ and modern Ruby

9. **RUBY-SMALL-METHODS-1** (`ruby-small-methods-1.toml`)
   - Domain: `ruby`
   - Enforcement: Structured
   - Requires methods to stay small and focused (10-20 lines) with single responsibility
   - Improves testability, reusability, and readability

10. **RUBY-CONCERNS-JUDICIOUSLY-1** (`ruby-concerns-judiciously-1.toml`)
    - Domain: `ruby:rails`
    - Enforcement: Structured
    - Limits Concerns (module mixins) to cross-cutting behavior used by 3+ models
    - Prevents concerns from becoming junk drawers for single-use or large logic blocks
    - Default: Extract large logic into service classes instead

### Input Validation
11. **RUBY-VALIDATION-LAYER-1** (`ruby-validation-layer-1.toml`)
    - Domain: `ruby:rails`
    - Enforcement: Structured
    - Requires validation at both controller (strong_parameters) and model (ActiveRecord validates) layers
    - Prevents invalid data from reaching the database
    - Dismisses client-side-only validation as insufficient

## Format & Consistency
All files follow the established TOML corpus format:
- `id` field with `RUBY-*` prefix (or `RUBY:RAILS-*` for Rails-specific rules)
- Clear `title`, `domain`, `layer`, and `enforcement` fields
- `[decision]` section with `question`, `default`, and `why`
- Multiple `[[option]]` entries with `id`, `label`, `directive`, and `why`
- Sensible enforcement tiers (mechanical for rules enforced by linters; structured for convention)

## Testing
- Ran `cargo test -p camerata-rules --lib`: **39/39 tests passed**
- Ran `cargo check -j2`: **All dependencies compiled, no errors**
- All 11 TOML files in `crates/rules/principles/ruby/` are successfully loaded by the corpus loader

## Files Created
- `crates/rules/principles/ruby/ruby-rails-parameterized-queries-1.toml`
- `crates/rules/principles/ruby/ruby-rails-strong-params-1.toml`
- `crates/rules/principles/ruby/ruby-no-hardcoded-secrets-1.toml`
- `crates/rules/principles/ruby/ruby-avoid-eval-send-1.toml`
- `crates/rules/principles/ruby/ruby-thin-controllers-1.toml`
- `crates/rules/principles/ruby/ruby-activerecord-scopes-1.toml`
- `crates/rules/principles/ruby/ruby-eager-load-associations-1.toml`
- `crates/rules/principles/ruby/ruby-frozen-string-literal-1.toml`
- `crates/rules/principles/ruby/ruby-small-methods-1.toml`
- `crates/rules/principles/ruby/ruby-concerns-judiciously-1.toml`
- `crates/rules/principles/ruby/ruby-validation-layer-1.toml`

## Verification
- No Rust source files touched (✓)
- No existing corpus rules edited (✓)
- `cargo check` green (✓)
- `cargo test -p camerata-rules` green (✓)
- TOML files follow exact format of sibling corpora (✓)
