# Ruby Testing Rule Grounding Report

All 7 rules in `crates/rules/principles/ruby/testing/` have been grounded against real, citable authority.

## Summary

- **Grounded:** 7
- **Ungrounded (draft):** 0
- **Demoted:** 0
- **Domain:** `ruby:testing`

## Authorities Used

| Authority | URL | Used for |
|-----------|-----|----------|
| RSpec Rails Docs | https://rspec.info/features/6-0/rspec-rails/directory-structure/ | Unit file location, integration location |
| RSpec Mocks Docs | https://rspec.info/features/3-12/rspec-mocks/verifying-doubles/ | Verified doubles |
| Better Specs | https://www.betterspecs.org/ | Naming, AAA structure, factories, mocking |
| RSpec Style Guide | https://rspec.rubystyle.guide/ | Naming conventions, let/subject ordering |
| rubocop-rspec (latest) | https://docs.rubocop.org/rubocop-rspec/latest/cops_rspec.html | Mechanical cop IDs for all mechanical rules |
| FactoryBot GitHub | https://github.com/thoughtbot/factory_bot | Factories over fixtures |
| Rails Guides — Testing | https://guides.rubyonrails.org/testing.html | Minitest layout (test/, models/, integration/, system/) |
| FastRuby.io blog | https://www.fastruby.io/blog/rspec/debug/how-to-debug-non-deterministic-specs.html | Flaky test prevention |
| Evil Martians blog | https://evilmartians.com/chronicles/flaky-tests-be-gone-long-lasting-relief-chronic-ci-retry-irritation | Flaky test prevention |

## Citation Table

| Rule ID | Verification | Source URL | Linter | Status |
|---------|-------------|------------|--------|--------|
| RUBY-TESTING-UNIT-FILE-LOCATION-1 | grounded | https://rspec.info/features/6-0/rspec-rails/directory-structure/ | rubocop-rspec: RSpec/SpecFilePathFormat | grounded |
| RUBY-TESTING-UNIT-FILE-LOCATION-1 | grounded | https://www.rubydoc.info/gems/rubocop-rspec/RuboCop/Cop/RSpec/SpecFilePathFormat | rubocop-rspec: RSpec/SpecFilePathFormat | grounded |
| RUBY-TESTING-UNIT-FILE-LOCATION-1 | grounded | https://guides.rubyonrails.org/testing.html | — | grounded |
| RUBY-TESTING-INTEGRATION-LOCATION-1 | grounded | https://rspec.info/features/6-0/rspec-rails/directory-structure/ | — | grounded |
| RUBY-TESTING-INTEGRATION-LOCATION-1 | grounded | https://www.codewithjason.com/difference-system-specs-feature-specs/ | — | grounded |
| RUBY-TESTING-INTEGRATION-LOCATION-1 | grounded | https://guides.rubyonrails.org/testing.html | — | grounded |
| RUBY-TESTING-DESCRIBE-NAMING-1 | grounded | https://www.betterspecs.org/ | — | grounded |
| RUBY-TESTING-DESCRIBE-NAMING-1 | grounded | https://rspec.rubystyle.guide/ | — | grounded |
| RUBY-TESTING-DESCRIBE-NAMING-1 | grounded | https://www.rubydoc.info/gems/rubocop-rspec/RuboCop/Cop/RSpec/DescribeClass | rubocop-rspec: RSpec/DescribeClass | grounded |
| RUBY-TESTING-DESCRIBE-NAMING-1 | grounded | https://www.rubydoc.info/gems/rubocop-rspec/RuboCop/Cop/RSpec/ContextWording | rubocop-rspec: RSpec/ContextWording | grounded |
| RUBY-TESTING-DESCRIBE-NAMING-1 | grounded | https://docs.rubocop.org/rubocop-rspec/latest/cops_rspec.html | rubocop-rspec: RSpec/ExampleWording | grounded |
| RUBY-TESTING-AAA-STRUCTURE-1 | grounded | https://www.fastruby.io/blog/testing/the-aaa-pattern-writing-robust-tests-for-any-project-with-confidence.html | — | grounded |
| RUBY-TESTING-AAA-STRUCTURE-1 | grounded | https://www.betterspecs.org/ | — | grounded |
| RUBY-TESTING-AAA-STRUCTURE-1 | grounded | https://rspec.rubystyle.guide/ | — | grounded |
| RUBY-TESTING-AAA-STRUCTURE-1 | grounded | https://docs.rubocop.org/rubocop-rspec/latest/cops_rspec.html | rubocop-rspec: RSpec/LetBeforeExamples | grounded |
| RUBY-TESTING-MOCK-AT-BOUNDARY-1 | grounded | https://rspec.info/features/3-12/rspec-mocks/verifying-doubles/ | — | grounded |
| RUBY-TESTING-MOCK-AT-BOUNDARY-1 | grounded | https://www.rubydoc.info/gems/rubocop-rspec/RuboCop/Cop/RSpec/VerifiedDoubles | rubocop-rspec: RSpec/VerifiedDoubles | grounded |
| RUBY-TESTING-MOCK-AT-BOUNDARY-1 | grounded | https://www.betterspecs.org/ | — | grounded |
| RUBY-TESTING-MOCK-AT-BOUNDARY-1 | grounded | https://github.com/rspec/rspec-mocks | — | grounded |
| RUBY-TESTING-DETERMINISTIC-NO-FLAKY-1 | grounded | https://www.fastruby.io/blog/rspec/debug/how-to-debug-non-deterministic-specs.html | — | grounded |
| RUBY-TESTING-DETERMINISTIC-NO-FLAKY-1 | grounded | https://evilmartians.com/chronicles/flaky-tests-be-gone-long-lasting-relief-chronic-ci-retry-irritation | — | grounded |
| RUBY-TESTING-DETERMINISTIC-NO-FLAKY-1 | grounded | https://docs.rubocop.org/rubocop-rspec/latest/cops_rspec.html | rubocop-rspec: RSpec/BeforeAfterAll | grounded |
| RUBY-TESTING-FACTORIES-OVER-FIXTURES-1 | grounded | https://github.com/thoughtbot/factory_bot | — | grounded |
| RUBY-TESTING-FACTORIES-OVER-FIXTURES-1 | grounded | https://github.com/thoughtbot/factory_bot/blob/main/GETTING_STARTED.md | — | grounded |
| RUBY-TESTING-FACTORIES-OVER-FIXTURES-1 | grounded | https://www.betterspecs.org/ | — | grounded |

## Mechanical Rule Bar

The following rules are `enforcement = "mechanical"` and each maps to a real rubocop-rspec linter cop:

| Rule ID | Cop ID | What it enforces |
|---------|--------|-----------------|
| RUBY-TESTING-UNIT-FILE-LOCATION-1 | `rubocop-rspec: RSpec/SpecFilePathFormat` | spec path mirrors source path; suffix is `_spec.rb` |
| RUBY-TESTING-DESCRIBE-NAMING-1 | `rubocop-rspec: RSpec/DescribeClass` | top-level describe is a constant |
| RUBY-TESTING-DESCRIBE-NAMING-1 | `rubocop-rspec: RSpec/ContextWording` | context starts with when/with/without |
| RUBY-TESTING-DESCRIBE-NAMING-1 | `rubocop-rspec: RSpec/ExampleWording` | it description does not begin with 'should'/'will' |
| RUBY-TESTING-MOCK-AT-BOUNDARY-1 | `rubocop-rspec: RSpec/VerifiedDoubles` | double() replaced by instance_double/class_double |
| RUBY-TESTING-AAA-STRUCTURE-1 | `rubocop-rspec: RSpec/LetBeforeExamples` | let blocks placed before examples |
| RUBY-TESTING-DETERMINISTIC-NO-FLAKY-1 | `rubocop-rspec: RSpec/BeforeAfterAll` | warns on before(:all) state leakage |

Rules with `enforcement = "structured"` (RUBY-TESTING-INTEGRATION-LOCATION-1, RUBY-TESTING-AAA-STRUCTURE-1, RUBY-TESTING-DETERMINISTIC-NO-FLAKY-1, RUBY-TESTING-FACTORIES-OVER-FIXTURES-1) have no single linter cop that covers the full rule; they emit into CONVENTIONS.md for code-review enforcement.

## Ungrounded Rules

None. All 7 rules carry at least one real URL source from an authoritative reference (RSpec official docs, betterspecs.org, rubocop-rspec docs, Rails Guides, or FactoryBot docs).

## Notes

- `verification = "verified"` was intentionally NOT set on any rule (gate-rejected per project policy; only a human maintainer may set that status).
- The cop `RSpec/SpecFilePathFormat` supersedes the older `RSpec/FilePath` (split in rubocop-rspec v2.24); this report uses the current name.
- betterspecs.org (https://www.betterspecs.org/) is a widely-cited community authority for RSpec style, referenced by multiple rubocop-rspec cop descriptions and the official RSpec style guide.
