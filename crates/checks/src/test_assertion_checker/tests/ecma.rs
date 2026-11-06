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
test('clicks without crashing', () => {
  render(<App />);
  fireEvent.click(screen.queryByText('go'));
});
test('computes a total', async function () {
  const t = await total([1, 2]);
});
";
    assert_eq!(
        one(src),
        vec!["is empty", "clicks without crashing", "computes a total"]
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
it('renders', () => { run(); });
";
    assert!(one(src).is_empty());
    let setup_only = "beforeEach(() => { reset(); });\nit('renders', () => { run(); });\n";
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
const plain = () => run(<App />);
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

#[test]
fn end_to_end_driver_commands_fail_when_the_page_does_not_match() {
    for body in [
        "cy.visit('/'); cy.contains('Welcome');",
        "await browser.url('/'); await $('#go').click();",
        "await element(by.id('go')).tap();",
    ] {
        let src = format!("it('loads', async () => {{ {body} }});\n");
        assert!(
            reported("cypress/e2e/home.cy.js", &src).is_empty(),
            "{body}"
        );
    }
}

#[test]
fn a_helper_name_declared_twice_is_followed_into_every_declaration() {
    let src = "\
describe('a', () => {
  const prepare = () => { expect(setup()).toBeTruthy(); };
  it('a', () => { prepare(); });
});
describe('b', () => {
  const prepare = () => setup();
  it('b', () => { prepare(); });
});
";
    // Which `prepare` each test calls is a scope question this check does not answer, so a
    // name that might reach an assertion counts as one.
    assert!(one(src).is_empty());
}

#[test]
fn safe_twin_a_render_only_test_is_a_smoke_test() {
    for body in [
        "render(<App />);",
        "const { container } = render(<App />);",
        "mount(<App />);",
        "const wrapper = shallow(<App />);",
        "rtl.render(<App />);",
    ] {
        let src = format!("it('renders without crashing', () => {{ {body} }});\n");
        assert!(one(&src).is_empty(), "{body}");
    }
    // Rendering plus another action is still unchecked.
    let acts = "it('clicks', () => { render(<App />); fireEvent.click(button); });\n";
    assert_eq!(one(acts), vec!["clicks"]);
}

#[test]
fn safe_twin_a_helper_from_a_relative_module_in_a_test_folder_may_assert() {
    let src = "import { runScenario } from './scenarios';\nit('a', () => { runScenario(); });\n";
    for path in [
        "tests/orders.test.ts",
        "src/__tests__/orders.test.ts",
        "test/orders.test.js",
    ] {
        assert!(reported(path, src).is_empty(), "{path}");
    }
    // Production code beside the test is not a helper module.
    let code = "import { total } from './orders';\nit('a', () => { total(1); });\n";
    assert_eq!(reported("src/orders.test.ts", code), vec!["a"]);
}

#[test]
fn safe_twin_commonjs_requires_are_imports() {
    for src in [
        "const helpers = require('./helpers');\nit('a', () => { helpers.run(x); });\n",
        "const { run } = require('./helpers');\nit('a', () => { run(x); });\n",
        "const { run: go } = require('../test-utils');\nit('a', () => { go(x); });\n",
        "const run = require('./helpers').run;\nit('a', () => { run(x); });\n",
        "const assert = require('assert');\nit('a', () => { assert.equal(1, 1); });\n",
    ] {
        assert!(one(src).is_empty(), "{src}");
    }
    // A required production module is not a helper.
    let code = "const { total } = require('../src/orders');\nit('a', () => { total(1); });\n";
    assert_eq!(one(code), vec!["a"]);
}
