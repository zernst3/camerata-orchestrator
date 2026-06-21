# JS Frameworks Rule Grounding Report

Family: `javascript/{next,express,vue,nest,angular}`
Date: 2026-06-20

## Summary

| Status | Count |
|--------|-------|
| Grounded | 26 |
| Ungrounded (draft) | 1 |
| Demoted | 0 |

### Ungrounded rules

- `JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1` — The smart/presentational (container/dumb) component pattern is a well-known architectural convention but does not appear as a numbered rule in the angular.dev style guide or as a named angular-eslint rule. No canonical authoritative URL was found; rule stays draft.

### Demoted rules

None. The one mechanical rule (`JAVASCRIPT-NEXT-DUAL-API-1`) has a real linter ID (`eslint: no-restricted-imports`) and a build-time enforcement mechanism (`server-only` package) documented in official Next.js docs.

---

## Rule Table

| rule-id | verification | source url | linter rule | status |
|---------|-------------|-----------|-------------|--------|
| JAVASCRIPT-ANGULAR-AVOID-LOGIC-IN-TEMPLATES-1 | grounded | https://angular.dev/style-guide | `@angular-eslint/template/no-call-expression` | grounded |
| JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1 | grounded | https://angular.dev/guide/di | `@angular-eslint/prefer-inject` | grounded |
| JAVASCRIPT-ANGULAR-LAZY-LOADING-ROUTES-1 | grounded | https://angular.dev/guide/routing/lazy-loading | — | grounded |
| JAVASCRIPT-ANGULAR-NO-DIRECT-DOM-MANIPULATION-1 | grounded | https://angular.dev/guide/components/dom-apis | — | grounded |
| JAVASCRIPT-ANGULAR-ONPUSH-CHANGE-DETECTION-1 | grounded | https://angular.dev/best-practices/skipping-subtrees | `@angular-eslint/prefer-on-push-component-change-detection` | grounded |
| JAVASCRIPT-ANGULAR-REACTIVE-FORMS-1 | grounded | https://angular.dev/guide/forms/reactive-forms | — | grounded |
| JAVASCRIPT-ANGULAR-ROUTE-GUARDS-1 | grounded | https://angular.dev/guide/routing/route-guards | — | grounded |
| JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1 | draft | — | — | ungrounded |
| JAVASCRIPT-ANGULAR-STANDALONE-COMPONENTS-1 | grounded | https://angular.dev/guide/components/anatomy-of-components | `@angular-eslint/prefer-standalone` | grounded |
| JAVASCRIPT-ANGULAR-SUBSCRIPTION-CLEANUP-1 | grounded | https://angular.dev/ecosystem/rxjs-interop/take-until-destroyed | `@angular-eslint/no-implicit-take-until-destroyed` | grounded |
| JAVASCRIPT-ANGULAR-TYPED-HTTPCLIENT-1 | grounded | https://angular.dev/guide/http/making-requests | — | grounded |
| JAVASCRIPT-EXPRESS-CENTRAL-ERROR-HANDLER-1 | grounded | https://expressjs.com/en/advanced/best-practice-performance.html | — | grounded |
| JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1 | grounded | https://expressjs.com/en/advanced/best-practice-security.html | — | grounded |
| JAVASCRIPT-EXPRESS-THIN-CONTROLLERS-1 | grounded | https://expressjs.com/en/advanced/best-practice-performance.html | — | grounded |
| JAVASCRIPT-EXPRESS-VALIDATE-INPUT-1 | grounded | https://expressjs.com/en/advanced/best-practice-security.html | — | grounded |
| JAVASCRIPT-VUE-COMPOSITION-SCRIPT-SETUP-1 | grounded | https://vuejs.org/api/sfc-script-setup | `eslint-plugin-vue: vue/component-api-style` | grounded |
| JAVASCRIPT-VUE-COMPUTED-OVER-METHODS-1 | grounded | https://vuejs.org/guide/essentials/computed.html | `eslint-plugin-vue: vue/no-side-effects-in-computed-properties` | grounded |
| JAVASCRIPT-VUE-NO-DIRECT-DOM-1 | grounded | https://vuejs.org/guide/essentials/reactivity-fundamentals.html | — | grounded |
| JAVASCRIPT-VUE-PROPS-DOWN-EVENTS-UP-1 | grounded | https://vuejs.org/guide/components/props.html | `eslint-plugin-vue: vue/no-mutating-props` | grounded |
| JAVASCRIPT-VUE-SCOPED-STYLES-1 | grounded | https://vuejs.org/style-guide/rules-essential#component-style-scoping | `eslint-plugin-vue: vue/enforce-style-attribute` | grounded |
| JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1 | grounded | https://pinia.vuejs.org/introduction.html | — | grounded |
| JAVASCRIPT-NEST-DTOS-VALIDATION-PIPE-1 | grounded | https://docs.nestjs.com/techniques/validation | — | grounded |
| JAVASCRIPT-NEST-GUARDS-FOR-AUTH-1 | grounded | https://docs.nestjs.com/guards | — | grounded |
| JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1 | grounded | https://docs.nestjs.com/interceptors | — | grounded |
| JAVASCRIPT-NEST-MODULES-PROVIDERS-DI-1 | grounded | https://docs.nestjs.com/providers | — | grounded |
| JAVASCRIPT-NEST-THIN-CONTROLLERS-DELEGATE-1 | grounded | https://docs.nestjs.com/controllers | — | grounded |
| JAVASCRIPT-NEXT-DUAL-API-1 | grounded | https://nextjs.org/docs/app/getting-started/server-and-client-components#preventing-environment-poisoning | `eslint: no-restricted-imports` | grounded |
| JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1 | grounded | https://nextjs.org/docs/app/api-reference/file-conventions/route-groups | — | grounded |

---

## Authorities used

- **Angular**: angular.dev/style-guide, angular.dev/guide/di, angular.dev/guide/routing/route-guards, angular.dev/guide/routing/lazy-loading, angular.dev/best-practices/skipping-subtrees, angular.dev/guide/forms/reactive-forms, angular.dev/guide/http/making-requests, angular.dev/ecosystem/rxjs-interop/take-until-destroyed, angular.dev/api/router/CanActivateFn; angular-eslint GitHub repository (prefer-inject, prefer-standalone, prefer-on-push-component-change-detection, no-implicit-take-until-destroyed, @angular-eslint/template/no-call-expression)
- **Express**: expressjs.com/en/advanced/best-practice-performance.html, expressjs.com/en/advanced/best-practice-security.html
- **Vue**: vuejs.org/style-guide (rules-essential, rules-strongly-recommended), vuejs.org/guide/essentials/computed, vuejs.org/guide/components/props, vuejs.org/guide/components/events, vuejs.org/api/sfc-script-setup; eslint.vuejs.org (vue/component-api-style, vue/no-side-effects-in-computed-properties, vue/no-mutating-props, vue/enforce-style-attribute); pinia.vuejs.org/introduction
- **NestJS**: docs.nestjs.com/controllers, docs.nestjs.com/providers, docs.nestjs.com/modules, docs.nestjs.com/guards, docs.nestjs.com/interceptors, docs.nestjs.com/techniques/validation
- **Next.js**: nextjs.org/docs/app/getting-started/server-and-client-components, nextjs.org/docs/app/api-reference/file-conventions/route-groups, nextjs.org/docs/app/getting-started/project-structure; eslint.org/docs/latest/rules/no-restricted-imports
