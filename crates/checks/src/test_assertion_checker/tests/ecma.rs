use super::reported;

fn one(src: &str) -> Vec<String> {
    reported("src/orders.test.tsx", src)
}

// ── positives ──────────────────────────────────────────────────────────────

#[test]
fn an_empty_test_and_a_smoke_test_are_reported() {
    let src = "\
import { render } from '@testing-library/react';
import { total } from '../src/orders';

it('is empty', () => {});
test('renders without crashing', () => {
  render(<App />);
});
test('computes a total', async function () {
  const t = await total([1, 2]);
});
";
    assert_eq!(
        one(src),
        vec!["is empty", "renders without crashing", "computes a total"]
    );
}

#[test]
fn only_and_concurrent_tests_run_and_are_read() {
    let src =
        "it.only('a', () => { run(); });\ntest.concurrent(`b`, async () => { await run(); });\n";
    assert_eq!(one(src), vec!["a", "b"]);
}

#[test]
fn framework_imports_of_it_and_test_are_read() {
    for module in ["vitest", "@jest/globals", "node:test", "bun:test", "mocha"] {
        let src = format!("import {{ it, test }} from '{module}';\nit('a', () => {{ run(); }});\n");
        assert_eq!(one(&src), vec!["a"], "{module}");
    }
}

#[test]
fn a_mock_that_is_never_checked_is_not_an_assertion() {
    let src = "\
test('click works', () => {
  const onClick = jest.fn();
  const { getByRole } = render(<Button onClick={onClick} />);
  fireEvent.click(screen.queryByText('x'));
});
";
    assert_eq!(one(src), vec!["click works"]);
}

#[test]
fn spec_and_test_folder_files_are_read() {
    for path in [
        "src/a.spec.js",
        "src/__tests__/a.jsx",
        "test/a.mjs",
        "e2e/a.ts",
    ] {
        assert_eq!(
            reported(path, "it('a', () => { run(); });\n"),
            vec!["a"],
            "{path}"
        );
    }
}

// ── safe twins and traps ───────────────────────────────────────────────────

#[test]
fn safe_twin_expect_assert_and_should_are_assertions() {
    for body in [
        "expect(total(1)).toBe(1);",
        "expect.assertions(1);",
        "await expect(load()).resolves.toEqual([]);",
        "assert.equal(total(1), 1);",
        "assert(total(1));",
        "total(1).should.equal(1);",
        "expectTypeOf(total).toBeFunction();",
        "if (total(1) !== 1) throw new Error('bad');",
        "await waitFor(() => screen.getByText('done'));",
        "screen.getByText('Title');",
        "await screen.findAllByRole('row');",
        "fail('not yet');",
        "sinon.assert.calledOnce(spy);",
        "td.verify(save(1));",
        "expectOrder(total(1));",
    ] {
        let src = format!("it('a', () => {{ {body} }});\n");
        assert!(one(&src).is_empty(), "{body}");
    }
}

#[test]
fn an_aliased_assertion_import_is_an_assertion() {
    for import in [
        "import { expect as e } from 'chai';",
        "import ok from 'node:assert';",
        "import * as a from 'assert';",
        "import { strict as s } from 'node:assert';",
    ] {
        let local = import
            .split_whitespace()
            .find(|w| ["e", "ok", "a", "s"].contains(w))
            .unwrap_or("e");
        let src = format!("{import}\nit('a', () => {{ {local}(run()); }});\n");
        assert!(one(&src).is_empty(), "{import}");
    }
}

#[test]
fn an_assertion_in_a_shared_hook_covers_every_test() {
    let src = "\
afterEach(() => { expect(console.error).not.toHaveBeenCalled(); });
it('renders', () => { render(<App />); });
";
    assert!(one(src).is_empty());
    let setup_only =
        "beforeEach(() => { reset(); });\nit('renders', () => { render(<App />); });\n";
    assert_eq!(one(setup_only), vec!["renders"]);
}

#[test]
fn a_callback_with_a_parameter_may_assert_through_it() {
    for src in [
        "test('ava', t => { run(); });",
        "test('tap', (t) => { run(); });",
        "it('mocha', function (done) { run(); });",
        "test('pw', async ({ page }) => { await page.goto('/'); });",
    ] {
        assert!(one(src).is_empty(), "{src}");
    }
}

#[test]
fn skipped_todo_and_table_tests_are_not_read() {
    for src in [
        "it.skip('a', () => { run(); });",
        "xit('a', () => { run(); });",
        "it.todo('a');",
        "test.each([1, 2])('a %i', () => { run(); });",
        "describe.skip('s', () => { it('a', () => { run(); }); });",
        "xdescribe('s', () => { it('a', () => { run(); }); });",
    ] {
        assert!(one(src).is_empty(), "{src}");
    }
    let running = "describe('s', () => { it('a', () => { run(); }); });";
    assert_eq!(one(running), vec!["a"]);
}

#[test]
fn a_custom_or_local_test_function_is_not_a_test_declaration() {
    for src in [
        "import { test } from './fixtures';\ntest('a', () => { run(); });",
        "import { test } from '@acme/test-kit';\ntest('a', () => { run(); });",
        "function it(name, fn) { fn(); }\nit('a', () => { run(); });",
        "const test = makeTest();\ntest('a', () => { run(); });",
    ] {
        assert!(one(src).is_empty(), "{src}");
    }
}

#[test]
fn a_member_call_named_test_is_not_a_test_declaration() {
    assert!(one("const ok = /^a/.test('x');\nregex.test('a', () => {});\n").is_empty());
}

#[test]
fn a_same_file_helper_that_asserts_is_an_assertion() {
    let src = "\
function confirm(node) { expect(node).toBeTruthy(); }
const renderIt = (ui) => { const r = render(ui); confirm(r); };
it('a', () => { renderIt(<App />); });
const plain = () => render(<App />);
it('b', () => { plain(); });
";
    assert_eq!(one(src), vec!["b"]);
}

#[test]
fn a_helper_from_a_test_support_module_may_assert() {
    for import in [
        "import { renderScenario } from './helpers';",
        "import { renderScenario } from '../test-utils';",
        "import { renderScenario } from '@/testing/render';",
        "import * as h from './support';",
    ] {
        let call = if import.contains("* as h") {
            "h.renderScenario();"
        } else {
            "renderScenario();"
        };
        let src = format!("{import}\nit('a', () => {{ {call} }});\n");
        assert!(one(&src).is_empty(), "{import}");
    }
}

#[test]
fn production_files_are_not_read() {
    assert!(reported("src/orders.ts", "it('a', () => { run(); });\n").is_empty());
}
