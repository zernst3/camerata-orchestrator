# Python Rule Grounding Report

Family: **python** (incl. django, flask subdirs)
Total rules: 25
Grounded: 24
Demoted: 1 (enforcement downgraded from `mechanical` to `prose`)
Ungrounded: 0

## Summary

All 25 rules in the Python family were grounded against authoritative sources:
PEP 8, PEP 484, official Python docs, Django docs, Flask docs, FastAPI docs,
SQLAlchemy docs, Ruff rules (docs.astral.sh/ruff/rules), and Bandit plugins.
One rule (PYTHON-ORM-EAGER-LOAD-1) was demoted from `mechanical` to `prose`
because its `qualifies` field describes a prose/test-based query-count check,
not a real linter rule — no static analysis tool mechanically detects N+1
patterns in Python.

## Demoted Rules

| Rule ID | Reason |
|---|---|
| PYTHON-ORM-EAGER-LOAD-1 | `qualifies` field describes a prose query-count test, not a real linter rule. Demoted `mechanical` → `prose`. |

## Ungrounded Rules

None.

## Citation Table

| Rule ID | Verification | Source URL | Linter Rule | Notes |
|---|---|---|---|---|
| PYTHON-DJANGO-CSRF-AUTH-1 | grounded | https://docs.djangoproject.com/en/6.0/ref/csrf/ | — | Also: Django auth/default/ for login_required |
| PYTHON-DJANGO-CSRF-AUTH-1 | grounded | https://docs.djangoproject.com/en/6.0/topics/auth/default/ | — | login_required / LoginRequiredMixin |
| PYTHON-DJANGO-FAT-MODEL-SERVICE-1 | grounded | https://docs.djangoproject.com/en/6.0/misc/design-philosophies/ | — | Django design philosophy: models contain domain logic |
| PYTHON-DJANGO-FORMS-SERIALIZERS-VALIDATE-1 | grounded | https://docs.djangoproject.com/en/6.0/topics/forms/ | — | is_valid(), cleaned_data |
| PYTHON-DJANGO-MIGRATIONS-CHECKED-IN-1 | grounded | https://docs.djangoproject.com/en/6.0/topics/migrations/ | — | makemigrations, migrate --check, commit migration files |
| PYTHON-DJANGO-ORM-PARAMETERIZED-1 | grounded | https://docs.djangoproject.com/en/6.0/topics/db/sql/ | — | params argument, cursor.execute parameterization |
| PYTHON-DJANGO-ORM-PARAMETERIZED-1 | grounded | https://docs.astral.sh/ruff/rules/hardcoded-sql-expression/ | Ruff: S608 | SQL injection detection |
| PYTHON-DJANGO-ORM-PARAMETERIZED-1 | grounded | https://bandit.readthedocs.io/en/latest/plugins/b608_hardcoded_sql_expressions.html | Bandit: B608 | SQL injection detection |
| PYTHON-DJANGO-SELECT-RELATED-1 | grounded | https://docs.djangoproject.com/en/6.0/topics/db/optimization/ | — | select_related, prefetch_related |
| PYTHON-DJANGO-SETTINGS-FROM-ENV-1 | grounded | https://docs.djangoproject.com/en/6.0/ref/settings/#secret-key | — | SECRET_KEY must be secret |
| PYTHON-DJANGO-SETTINGS-FROM-ENV-1 | grounded | https://bandit.readthedocs.io/en/latest/plugins/b105_hardcoded_password_string.html | Bandit: B105 | hardcoded secrets |
| PYTHON-DJANGO-SETTINGS-FROM-ENV-1 | grounded | https://docs.astral.sh/ruff/rules/hardcoded-password-string/ | Ruff: S105 | hardcoded secrets |
| PYTHON-EXPLICIT-IMPORTS-1 | grounded | https://peps.python.org/pep-0008/#imports | — | Wildcard imports should be avoided |
| PYTHON-EXPLICIT-IMPORTS-1 | grounded | https://docs.astral.sh/ruff/rules/undefined-local-with-import-star/ | Ruff: F403 | wildcard import detection |
| PYTHON-FASTAPI-AUTH-DEPENDENCY-1 | grounded | https://fastapi.tiangolo.com/tutorial/dependencies/ | — | Depends for security, auth |
| PYTHON-FASTAPI-AUTH-DEPENDENCY-1 | grounded | https://fastapi.tiangolo.com/tutorial/security/ | — | OAuth2, JWT, Depends for auth |
| PYTHON-FASTAPI-DI-SESSION-1 | grounded | https://fastapi.tiangolo.com/tutorial/sql-databases/ | — | per-request session via Depends yield |
| PYTHON-FASTAPI-PYDANTIC-MODELS-1 | grounded | https://fastapi.tiangolo.com/tutorial/request-body/ | — | Pydantic models for validation |
| PYTHON-FASTAPI-PYDANTIC-MODELS-1 | grounded | https://fastapi.tiangolo.com/tutorial/response-model/ | — | response_model filters output |
| PYTHON-FLASK-APP-FACTORY-1 | grounded | https://flask.palletsprojects.com/en/stable/patterns/appfactories/ | — | create_app pattern |
| PYTHON-FLASK-APP-FACTORY-1 | grounded | https://flask.palletsprojects.com/en/stable/blueprints/ | — | Blueprints |
| PYTHON-FLASK-AUTH-ON-PROTECTED-ROUTES-1 | grounded | https://flask-login.readthedocs.io/en/latest/ | — | login_required decorator |
| PYTHON-FLASK-CONFIG-FROM-ENV-1 | grounded | https://flask.palletsprojects.com/en/stable/config/ | — | SECRET_KEY, env vars, config best practices |
| PYTHON-FLASK-CONFIG-FROM-ENV-1 | grounded | https://bandit.readthedocs.io/en/latest/plugins/b105_hardcoded_password_string.html | Bandit: B105 | hardcoded secrets |
| PYTHON-FLASK-CONFIG-FROM-ENV-1 | grounded | https://docs.astral.sh/ruff/rules/hardcoded-password-string/ | Ruff: S105 | hardcoded secrets |
| PYTHON-FLASK-ERROR-HANDLERS-1 | grounded | https://flask.palletsprojects.com/en/stable/errorhandling/ | — | errorhandler decorator, Blueprint error handlers |
| PYTHON-FLASK-PARAMETERIZED-SQL-1 | grounded | https://docs.astral.sh/ruff/rules/hardcoded-sql-expression/ | Ruff: S608 | SQL injection detection |
| PYTHON-FLASK-PARAMETERIZED-SQL-1 | grounded | https://bandit.readthedocs.io/en/latest/plugins/b608_hardcoded_sql_expressions.html | Bandit: B608 | SQL injection detection |
| PYTHON-FLASK-REQUEST-VALIDATION-1 | grounded | https://flask.palletsprojects.com/en/stable/api/#flask.Request.json | — | raw untrusted client data |
| PYTHON-FLASK-SERVICE-LAYER-1 | grounded | https://flask.palletsprojects.com/en/stable/patterns/appfactories/ | — | separating concerns, testability |
| PYTHON-NO-BARE-EXCEPT-1 | grounded | https://docs.astral.sh/ruff/rules/bare-except/ | Ruff: E722 | bare-except detection |
| PYTHON-NO-BARE-EXCEPT-1 | grounded | https://docs.astral.sh/ruff/rules/blind-exception/ | Ruff: BLE001 | blind-exception detection |
| PYTHON-NO-BARE-EXCEPT-1 | grounded | https://peps.python.org/pep-0008/#exception-handling | — | PEP 8 on bare except clause |
| PYTHON-NO-BLOCKING-IO-ASYNC-1 | grounded | https://docs.python.org/3/library/asyncio-eventloop.html#asyncio.loop.run_in_executor | — | run_in_executor for blocking IO offload |
| PYTHON-NO-BLOCKING-IO-ASYNC-1 | grounded | https://docs.astral.sh/ruff/rules/blocking-sleep-in-async-function/ | Ruff: ASYNC251 | blocking sleep in async |
| PYTHON-NO-BLOCKING-IO-ASYNC-1 | grounded | https://docs.astral.sh/ruff/rules/blocking-open-call-in-async-function/ | Ruff: ASYNC230 | blocking file open in async |
| PYTHON-NO-BLOCKING-IO-ASYNC-1 | grounded | https://docs.astral.sh/ruff/rules/blocking-http-call-in-async-function/ | Ruff: ASYNC210 | blocking HTTP call in async |
| PYTHON-ORM-EAGER-LOAD-1 | grounded | https://docs.sqlalchemy.org/en/20/orm/queryguide/relationships.html | — | DEMOTED: enforcement mechanical → prose; selectinload/joinedload N+1 |
| PYTHON-PARAMETERIZED-SQL-1 | grounded | https://docs.astral.sh/ruff/rules/hardcoded-sql-expression/ | Ruff: S608 | SQL injection detection |
| PYTHON-PARAMETERIZED-SQL-1 | grounded | https://bandit.readthedocs.io/en/latest/plugins/b608_hardcoded_sql_expressions.html | Bandit: B608 | SQL injection detection |
| PYTHON-SERVICE-LAYER-1 | grounded | https://docs.djangoproject.com/en/6.0/misc/design-philosophies/ | — | Django design philosophy: thin views |
| PYTHON-SETTINGS-FROM-ENV-1 | grounded | https://bandit.readthedocs.io/en/latest/plugins/b105_hardcoded_password_string.html | Bandit: B105 | hardcoded secrets |
| PYTHON-SETTINGS-FROM-ENV-1 | grounded | https://docs.astral.sh/ruff/rules/hardcoded-password-default/ | Ruff: S107 | hardcoded-password-default |
| PYTHON-TYPE-HINTS-1 | grounded | https://peps.python.org/pep-0484/ | — | PEP 484 Type Hints |
| PYTHON-TYPE-HINTS-1 | grounded | https://docs.astral.sh/ruff/rules/missing-type-function-argument/ | Ruff: ANN001 | missing function arg type annotation |
| PYTHON-TYPE-HINTS-1 | grounded | https://docs.astral.sh/ruff/rules/missing-return-type-undocumented-public-function/ | Ruff: ANN201 | missing return type annotation |

## Authorities Used

- **PEP 8** — https://peps.python.org/pep-0008/ (imports, exception handling)
- **PEP 484** — https://peps.python.org/pep-0484/ (type hints)
- **Ruff rules** — https://docs.astral.sh/ruff/rules/ (E722, BLE001, F403, S105, S107, S608, ANN001, ANN201, ASYNC210, ASYNC230, ASYNC251)
- **Bandit** — https://bandit.readthedocs.io/ (B105, B608)
- **Django docs** — https://docs.djangoproject.com/ (CSRF, auth, forms, migrations, ORM, optimization, settings, design philosophies)
- **Flask docs** — https://flask.palletsprojects.com/ (config, error handling, blueprints, app factories)
- **Flask-Login** — https://flask-login.readthedocs.io/ (login_required)
- **FastAPI docs** — https://fastapi.tiangolo.com/ (dependencies, security, SQL databases, request body, response model)
- **SQLAlchemy docs** — https://docs.sqlalchemy.org/ (relationship loading, N+1)
- **Python asyncio docs** — https://docs.python.org/3/library/asyncio-eventloop.html (run_in_executor)
