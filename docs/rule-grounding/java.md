# Java Rule Grounding Report

Grounding pass date: 2026-06-20
Family: `java` (includes `java/spring` subdirectory)
Total rules: 21
Grounded: 21
Ungrounded: 0
Demoted (mechanical → prose): 3

## Summary

### Ungrounded Rules
None. All 21 rules were grounded against real authoritative sources.

### Demoted Rules (enforcement changed from `mechanical` to `prose`)
| Rule ID | Reason |
|---------|--------|
| `JAVA-CONSTRUCTOR-INJECTION-1` | `qualifies` claims Checkstyle/Semgrep flag @Autowired fields; no standard named Checkstyle rule with a verifiable ID was found for this exact check. Grounded against Spring docs; enforcement changed to `prose`. |
| `JAVA-JPA-EAGER-LOAD-1` | `qualifies` says "code review and performance testing" — no real linter rule cited; was already aspirational. Grounded against Spring Data JPA docs; enforcement changed to `prose`. |
| `JAVA-SPRING-CONSTRUCTOR-INJECTION-1` | Same as JAVA-CONSTRUCTOR-INJECTION-1: no named Checkstyle rule specifically flags @Autowired on fields. Grounded against Spring docs; enforcement changed to `prose`. |
| `JAVA-SPRING-VALID-REQUEST-VALIDATION-1` | `qualifies` claims "Checkstyle or Semgrep rule" but no standard named Checkstyle rule ID was found for this check. Grounded against Spring MVC docs; enforcement changed to `prose`. |

---

## Citation Table

| Rule ID | Verification | Source URL | Linter Rule | Status |
|---------|-------------|------------|-------------|--------|
| JAVA-CONSTRUCTOR-INJECTION-1 | grounded | https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html | Checkstyle: VisibilityModifier | grounded (demoted) |
| JAVA-EXCEPTION-HANDLING-1 | grounded | https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html#de-method-might-ignore-exception-de-might-ignore | SpotBugs: DE_MIGHT_IGNORE | grounded |
| JAVA-EXCEPTION-HANDLING-1 | grounded | https://checkstyle.org/checks/blocks/emptycatchblock.html | Checkstyle: EmptyCatchBlock | grounded |
| JAVA-EXCEPTION-HANDLING-1 | grounded | https://google.github.io/styleguide/javaguide.html#s6.2-caught-exceptions | — | grounded |
| JAVA-IMMUTABILITY-FINAL-1 | grounded | https://checkstyle.org/checks/design/finalclass.html | Checkstyle: FinalClass | grounded |
| JAVA-IMMUTABILITY-FINAL-1 | grounded | https://checkstyle.org/checks/design/visibilitymodifier.html | Checkstyle: VisibilityModifier | grounded |
| JAVA-JPA-EAGER-LOAD-1 | grounded | https://docs.spring.io/spring-data/jpa/reference/jpa/query-methods.html#jpa.entity-graph | — | grounded (demoted) |
| JAVA-LAYERING-CONTROLLER-SERVICE-REPO-1 | grounded | https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html | — | grounded |
| JAVA-LOGGING-STRUCTURED-1 | grounded | https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html#systemprintln | PMD: SystemPrintln | grounded |
| JAVA-NO-HARDCODED-SECRETS-1 | grounded | https://find-sec-bugs.github.io/bugs.htm#HARD_CODE_PASSWORD | SpotBugs/FindSecBugs: HARD_CODE_PASSWORD | grounded |
| JAVA-NO-HARDCODED-SECRETS-1 | grounded | https://find-sec-bugs.github.io/bugs.htm#HARD_CODE_KEY | SpotBugs/FindSecBugs: HARD_CODE_KEY | grounded |
| JAVA-OPTIONAL-OVER-NULL-1 | grounded | https://errorprone.info/bugpatterns | Error Prone: NullOptional | grounded |
| JAVA-PACKAGE-BY-FEATURE-1 | grounded | https://pmd.github.io/pmd/pmd_rules_java_design.html#loosepackagecoupling | PMD: LoosePackageCoupling | grounded |
| JAVA-RESOURCE-MANAGEMENT-1 | grounded | https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html#os-method-may-fail-to-close-stream-on-exception-os-open-stream | SpotBugs: OS_OPEN_STREAM | grounded |
| JAVA-RESOURCE-MANAGEMENT-1 | grounded | https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html#odr-method-may-fail-to-close-database-resource-on-exception-odr-open-database-resource | SpotBugs: ODR_OPEN_DATABASE_RESOURCE | grounded |
| JAVA-RESOURCE-MANAGEMENT-1 | grounded | https://errorprone.info/bugpattern/StreamResourceLeak | Error Prone: StreamResourceLeak | grounded |
| JAVA-SMALL-INTERFACES-1 | grounded | https://pmd.github.io/pmd/pmd_rules_java_design.html#excessivepubliccount | PMD: ExcessivePublicCount | grounded |
| JAVA-SQL-PARAMETERIZED-1 | grounded | https://find-sec-bugs.github.io/bugs.htm#SQL_INJECTION_JDBC | SpotBugs/FindSecBugs: SQL_INJECTION_JDBC | grounded |
| JAVA-SPRING-CONFIGURATION-PROPERTIES-1 | grounded | https://docs.spring.io/spring-boot/reference/features/external-config.html#features.external-config.typesafe-configuration-properties | — | grounded |
| JAVA-SPRING-CONSTRUCTOR-INJECTION-1 | grounded | https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html | Checkstyle: VisibilityModifier | grounded (demoted) |
| JAVA-SPRING-DTO-BOUNDARY-1 | grounded | https://docs.spring.io/spring-framework/reference/web/webmvc/mvc-controller/ann-validation.html | — | grounded |
| JAVA-SPRING-LAYERED-ARCHITECTURE-1 | grounded | https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html | — | grounded |
| JAVA-SPRING-LAYERED-ARCHITECTURE-1 | grounded | https://docs.spring.io/spring-framework/reference/data-access/transaction/declarative/annotations.html | — | grounded |
| JAVA-SPRING-METHOD-SECURITY-1 | grounded | https://docs.spring.io/spring-security/reference/servlet/authorization/method-security.html | — | grounded |
| JAVA-SPRING-NO-NPLUS1-FETCH-JOIN-1 | grounded | https://docs.spring.io/spring-data/jpa/reference/jpa/query-methods.html#jpa.entity-graph | — | grounded |
| JAVA-SPRING-THIN-CONTROLLERS-1 | grounded | https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html | — | grounded |
| JAVA-SPRING-TRANSACTIONAL-SERVICE-LAYER-1 | grounded | https://docs.spring.io/spring-framework/reference/data-access/transaction/declarative/annotations.html | — | grounded |
| JAVA-SPRING-VALID-REQUEST-VALIDATION-1 | grounded | https://docs.spring.io/spring-framework/reference/web/webmvc/mvc-controller/ann-validation.html | — | grounded (demoted) |

---

## Authorities Used

- **SpotBugs bug descriptions**: https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html
- **Find Security Bugs patterns**: https://find-sec-bugs.github.io/bugs.htm
- **Google Java Style Guide**: https://google.github.io/styleguide/javaguide.html
- **Checkstyle checks**: https://checkstyle.org/checks/
- **Error Prone bug patterns**: https://errorprone.info/bugpatterns
- **PMD Java rules (best practices)**: https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html
- **PMD Java rules (design)**: https://pmd.github.io/pmd/pmd_rules_java_design.html
- **Spring Framework Reference — Dependency Injection**: https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html
- **Spring Framework Reference — Stereotype annotations**: https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html
- **Spring Framework Reference — @Transactional**: https://docs.spring.io/spring-framework/reference/data-access/transaction/declarative/annotations.html
- **Spring Framework Reference — Validation**: https://docs.spring.io/spring-framework/reference/web/webmvc/mvc-controller/ann-validation.html
- **Spring Boot Reference — @ConfigurationProperties**: https://docs.spring.io/spring-boot/reference/features/external-config.html
- **Spring Security Reference — Method Security**: https://docs.spring.io/spring-security/reference/servlet/authorization/method-security.html
- **Spring Data JPA Reference — @EntityGraph**: https://docs.spring.io/spring-data/jpa/reference/jpa/query-methods.html

---

## Notes on Grounding Decisions

**JAVA-CONSTRUCTOR-INJECTION-1 and JAVA-SPRING-CONSTRUCTOR-INJECTION-1**: The `qualifies` field claimed "Checkstyle or Semgrep rules that flag @Autowired fields." No standard Checkstyle rule with a known published ID specifically flags `@Autowired` on instance fields. `Checkstyle: VisibilityModifier` enforces field visibility constraints (private/final) but does not directly flag `@Autowired` field injection. Enforcement changed to `prose`; Spring Framework reference docs confirm the recommendation for constructor injection.

**JAVA-JPA-EAGER-LOAD-1**: The `qualifies` field said "Enforced by code review and performance testing" — aspirational language with no linter. No known PMD/SpotBugs rule specifically detects `FetchType.EAGER` usage as a violation. Enforcement demoted to `prose`; Spring Data JPA docs confirm LAZY default and `@EntityGraph` as the accepted mechanism.

**JAVA-SPRING-VALID-REQUEST-VALIDATION-1**: The `qualifies` field claimed "Checkstyle or Semgrep rule." No standard named Checkstyle rule with a published ID was found for flagging missing `@Valid` on `@RequestBody` parameters. Enforcement changed to `prose`; Spring MVC validation docs confirm the `@Valid`/`MethodArgumentNotValidException` pattern.

**JAVA-OPTIONAL-OVER-NULL-1**: No Checkstyle or SpotBugs rule specifically mandates `Optional<T>` return types over `null`. Error Prone's `NullOptional` pattern was the closest match (flags passing `null` to `Optional` parameters). Enforcement remains `prose` (already was).
