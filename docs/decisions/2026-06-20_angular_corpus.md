# 2026-06-20 Angular Framework Rule Corpus

## Summary

Added a comprehensive Angular framework rule corpus to the camerata-rules workspace, establishing best practices and architectural patterns for Angular applications.

## Rules Added (10 total)

1. **JAVASCRIPT-ANGULAR-STANDALONE-COMPONENTS-1**
   - Components are standalone (modern Angular) with explicit dependency imports
   - Enforcement: structured
   - Covers the modern Angular 14+ standalone component pattern and explicit imports

2. **JAVASCRIPT-ANGULAR-ONPUSH-CHANGE-DETECTION-1**
   - Components use OnPush change detection by default for performance
   - Enforcement: structured
   - Ensures performant change detection strategy across the component tree

3. **JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1**
   - Components follow the smart (container) vs. presentational pattern
   - Enforcement: structured
   - Establishes clear separation of concerns across the component hierarchy

4. **JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1**
   - Services are injected via constructor parameters or the inject() function, never manually instantiated
   - Enforcement: structured
   - Guarantees proper dependency injection and lifecycle management

5. **JAVASCRIPT-ANGULAR-SUBSCRIPTION-CLEANUP-1**
   - Observable subscriptions are cleaned up using takeUntilDestroyed or the async pipe
   - Enforcement: structured
   - Prevents memory leaks and dangling subscriptions

6. **JAVASCRIPT-ANGULAR-REACTIVE-FORMS-1**
   - Complex forms use reactive (FormBuilder, FormGroup) over template-driven forms
   - Enforcement: structured
   - Ensures form logic is explicit, testable, and maintainable

7. **JAVASCRIPT-ANGULAR-TYPED-HTTPCLIENT-1**
   - HTTP requests are made through typed services, not directly from components
   - Enforcement: structured
   - Enforces data-access layering and reusability

8. **JAVASCRIPT-ANGULAR-ROUTE-GUARDS-1**
   - Protected routes are guarded by canActivate guards, not checked in component ngOnInit
   - Enforcement: structured
   - Establishes authorization at the route boundary before component instantiation

9. **JAVASCRIPT-ANGULAR-LAZY-LOADING-ROUTES-1**
   - Feature routes are lazy-loaded to reduce the initial bundle
   - Enforcement: structured
   - Optimizes bundle size and time-to-interactive

10. **JAVASCRIPT-ANGULAR-AVOID-LOGIC-IN-TEMPLATES-1**
    - Templates are simple and logic-free; complex conditions and computations live in the component
    - Enforcement: prose
    - Maintains template readability, testability, and performance

## Domain

All rules use domain `javascript:angular` and layer `framework`.

## Validation

All rules were validated via `cargo test -p camerata-rules`:
- Test output: 39 tests passed
- Build status: successful
- Rules load without errors

## Files Added

- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-avoid-logic-in-templates-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-di-constructor-or-inject-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-lazy-loading-routes-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-no-direct-dom-manipulation-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-onpush-change-detection-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-reactive-forms-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-route-guards-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-smart-presentational-pattern-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-standalone-components-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-subscription-cleanup-1.toml`
- `/tmp/camerata-fw-ng/crates/rules/principles/javascript/angular/javascript-angular-typed-httpclient-1.toml`

## Coverage

The corpus covers major Angular best practices:
- Modern component patterns (standalone, OnPush, smart/presentational)
- Dependency injection and service layer organization
- Lifecycle and subscription management
- Form handling strategies
- HTTP data access patterns
- Route protection and optimization
- DOM manipulation and template patterns

All rules follow the established camerata-rules TOML format with decision questions, default options, and documented alternatives.
