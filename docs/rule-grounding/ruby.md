# Ruby Rule Grounding Report

All 20 Ruby rules (11 root, 9 rails subdir) have been grounded against real, citable authority. Zero rules are ungrounded. Zero rules were demoted.

## Summary

- **Grounded:** 20
- **Ungrounded:** 0
- **Demoted:** 0

## Citation Table

| Rule ID | Verification | Source URL | Linter Rule | Status |
|---------|-------------|------------|-------------|--------|
| RUBY-ACTIVERECORD-SCOPES-1 | grounded | https://guides.rubyonrails.org/active_record_querying.html#scopes | — | grounded |
| RUBY-AVOID-EVAL-SEND-1 | grounded | https://brakemanscanner.org/docs/warning_types/dangerous_eval/ | Brakeman: Dangerous Evaluation | grounded |
| RUBY-AVOID-EVAL-SEND-1 | grounded | https://brakemanscanner.org/docs/warning_types/dangerous_send/ | Brakeman: Dangerous Send | grounded |
| RUBY-CONCERNS-JUDICIOUSLY-1 | grounded | https://guides.rubyonrails.org/engines.html#concerns | — | grounded |
| RUBY-EAGER-LOAD-ASSOCIATIONS-1 | grounded | https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations | — | grounded |
| RUBY-EAGER-LOAD-ASSOCIATIONS-1 | grounded | https://github.com/flyerhzm/bullet | Bullet: N+1 query detection | grounded |
| RUBY-FROZEN-STRING-LITERAL-1 | grounded | https://github.com/rubocop/rubocop/blob/master/lib/rubocop/cop/style/frozen_string_literal_comment.rb | RuboCop: Style/FrozenStringLiteralComment | grounded |
| RUBY-FROZEN-STRING-LITERAL-1 | grounded | https://rubystyle.guide/#magic-comments | — | grounded |
| RUBY-NO-HARDCODED-SECRETS-1 | grounded | https://github.com/gitleaks/gitleaks | Gitleaks: secret pattern detection | grounded |
| RUBY-NO-HARDCODED-SECRETS-1 | grounded | https://guides.rubyonrails.org/security.html#custom-credentials | — | grounded |
| RUBY-RAILS-PARAMETERIZED-QUERIES-1 | grounded | https://brakemanscanner.org/docs/warning_types/sql_injection/ | Brakeman: SQL Injection | grounded |
| RUBY-RAILS-PARAMETERIZED-QUERIES-1 | grounded | https://guides.rubyonrails.org/security.html#sql-injection | — | grounded |
| RUBY-RAILS-STRONG-PARAMS-1 (root) | grounded | https://brakemanscanner.org/docs/warning_types/mass_assignment/ | Brakeman: Mass Assignment | grounded |
| RUBY-RAILS-STRONG-PARAMS-1 (root) | grounded | https://guides.rubyonrails.org/action_controller_overview.html#strong-parameters | — | grounded |
| RUBY-SMALL-METHODS-1 | grounded | https://rubystyle.guide/#short-methods | — | grounded |
| RUBY-SMALL-METHODS-1 | grounded | https://docs.rubocop.org/rubocop/1.75/cops_metrics.html#metricsmethodlength | RuboCop: Metrics/MethodLength | grounded |
| RUBY-THIN-CONTROLLERS-1 | grounded | https://guides.rubyonrails.org/action_controller_overview.html | — | grounded |
| RUBY-VALIDATION-LAYER-1 | grounded | https://guides.rubyonrails.org/active_record_validations.html | — | grounded |
| RUBY-VALIDATION-LAYER-1 | grounded | https://guides.rubyonrails.org/action_controller_overview.html#strong-parameters | — | grounded |
| RUBY-RAILS-BACKGROUND-JOBS-1 | grounded | https://guides.rubyonrails.org/active_job_basics.html | — | grounded |
| RUBY-RAILS-CSRF-1 | grounded | https://guides.rubyonrails.org/security.html#cross-site-request-forgery-csrf | — | grounded |
| RUBY-RAILS-EAGER-LOAD-1 | grounded | https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations | — | grounded |
| RUBY-RAILS-EAGER-LOAD-1 | grounded | https://github.com/flyerhzm/bullet | Bullet: N+1 query detection | grounded |
| RUBY-RAILS-MODEL-VALIDATIONS-1 | grounded | https://guides.rubyonrails.org/active_record_validations.html | — | grounded |
| RUBY-RAILS-NO-SECRETS-IN-CODE-1 | grounded | https://github.com/gitleaks/gitleaks | Gitleaks: secret pattern detection | grounded |
| RUBY-RAILS-NO-SECRETS-IN-CODE-1 | grounded | https://guides.rubyonrails.org/security.html#custom-credentials | — | grounded |
| RUBY-RAILS-NO-STRING-SQL-1 | grounded | https://brakemanscanner.org/docs/warning_types/sql_injection/ | Brakeman: SQL Injection | grounded |
| RUBY-RAILS-NO-STRING-SQL-1 | grounded | https://guides.rubyonrails.org/security.html#sql-injection | — | grounded |
| RUBY-RAILS-SCOPES-1 | grounded | https://guides.rubyonrails.org/active_record_querying.html#scopes | — | grounded |
| RUBY-RAILS-SKINNY-CONTROLLERS-1 | grounded | https://guides.rubyonrails.org/action_controller_overview.html | — | grounded |
| RUBY-RAILS-STRONG-PARAMS-1 (rails) | grounded | https://brakemanscanner.org/docs/warning_types/mass_assignment/ | Brakeman: Mass Assignment | grounded |
| RUBY-RAILS-STRONG-PARAMS-1 (rails) | grounded | https://guides.rubyonrails.org/action_controller_overview.html#strong-parameters | — | grounded |

## Ungrounded Rules

None.

## Demoted Rules

None. All mechanical rules that claimed linter enforcement were backed by real linters (Brakeman for security checks, RuboCop for style, Gitleaks for secret detection, Bullet for N+1 detection).

## Authorities Consulted

- **Rails Guides** (guides.rubyonrails.org): Active Record Query Interface, Active Record Validations, Action Controller Overview, Active Job Basics, Securing Rails Applications, Engines
- **Brakeman** (brakemanscanner.org): Warning types for Dangerous Evaluation, Dangerous Send, SQL Injection, Mass Assignment
- **RuboCop** (docs.rubocop.org, github.com/rubocop/rubocop): Style/FrozenStringLiteralComment cop, Metrics/MethodLength cop
- **Ruby Style Guide** (rubystyle.guide): Short Methods (#short-methods), Magic Comments (#magic-comments)
- **Bullet gem** (github.com/flyerhzm/bullet): N+1 query detection
- **Gitleaks** (github.com/gitleaks/gitleaks): Secret pattern detection in git repositories

## Notes on Mechanical Enforcement

All eight rules with `enforcement = "mechanical"` or a `qualifies` field map to real linter tools:

- `RUBY-AVOID-EVAL-SEND-1`: Brakeman Dangerous Evaluation + Dangerous Send
- `RUBY-EAGER-LOAD-ASSOCIATIONS-1`: Bullet gem (N+1 detection)
- `RUBY-FROZEN-STRING-LITERAL-1`: RuboCop Style/FrozenStringLiteralComment
- `RUBY-NO-HARDCODED-SECRETS-1`: Gitleaks
- `RUBY-RAILS-PARAMETERIZED-QUERIES-1`: Brakeman SQL Injection
- `RUBY-RAILS-STRONG-PARAMS-1` (root): Brakeman Mass Assignment
- `RUBY-RAILS-NO-SECRETS-IN-CODE-1`: Gitleaks
- `RUBY-RAILS-NO-STRING-SQL-1`: Brakeman SQL Injection
- `RUBY-RAILS-STRONG-PARAMS-1` (rails subdir): Brakeman Mass Assignment

Note: `RUBY-RAILS-NO-STRING-SQL-1` claims "a Rubocop-Rails rule" in its `qualifies` text. No specific rubocop-rails cop for SQL string interpolation was found in the rubocop-rails cop directory (the closest is `Rails/WhereRange` for style, not injection). Brakeman SQL Injection is the real enforcing tool. The claim about a rubocop-rails rule is left in the `qualifies` text (not our field to edit), but the sourced linter is Brakeman.
