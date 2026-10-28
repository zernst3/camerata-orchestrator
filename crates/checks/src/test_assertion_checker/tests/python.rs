use super::{reported, run};

fn one(src: &str) -> Vec<String> {
    reported("tests/test_orders.py", src)
}

// ── positives ──────────────────────────────────────────────────────────────

#[test]
fn empty_and_smoke_tests_are_reported() {
    let src = "\
from myapp.orders import total


def test_empty():
    pass


def test_docstring_only():
    \"\"\"Totals add up.\"\"\"


async def test_calls_the_code(client):
    await total([1, 2])


def helper():
    return 1
";
    assert_eq!(
        one(src),
        vec!["test_empty", "test_docstring_only", "test_calls_the_code"]
    );
}

#[test]
fn test_class_methods_and_unittest_methods_are_read() {
    let src = "\
import unittest
from unittest import mock


class TestOrders:
    def test_a(self):
        self.client.get('/orders')

    def setup_method(self):
        pass


class OrdersCase(unittest.TestCase):
    def test_b(self):
        with mock.patch('myapp.orders.save'):
            run()


class Helper:
    def test_not_collected(self):
        pass
";
    assert_eq!(one(src), vec!["test_a", "test_b"]);
}

#[test]
fn every_collected_module_name_is_read() {
    let src = "def test_x():\n    run()\n";
    for path in [
        "test_a.py",
        "pkg/a_test.py",
        "app/tests.py",
        "tests/test_a.py",
    ] {
        assert_eq!(reported(path, src), vec!["test_x"], "{path}");
    }
    for path in ["tests/helpers.py", "tests/conftest.py", "app/orders.py"] {
        assert!(reported(path, src).is_empty(), "{path}");
    }
}

#[test]
fn marks_patches_and_hypothesis_decorators_do_not_assert() {
    let src = "\
import pytest
from unittest.mock import patch
from hypothesis import given, strategies as st


@pytest.mark.parametrize('n', [1, 2])
def test_a(n):
    run(n)


@pytest.mark.django_db
@patch('myapp.save')
def test_b(save):
    run()


@given(st.integers())
def test_c(n):
    run(n)
";
    assert_eq!(one(src), vec!["test_a", "test_b", "test_c"]);
}

// ── safe twins and traps ───────────────────────────────────────────────────

#[test]
fn safe_twin_assert_raise_and_framework_checks_are_assertions() {
    for body in [
        "assert total([1]) == 1",
        "raise AssertionError('no')",
        "with pytest.raises(ValueError):\n        total(None)",
        "with pytest.warns(UserWarning):\n        total(None)",
        "self.assertEqual(total([1]), 1)",
        "save.assert_called_once_with(1)",
        "check_total(total([1]))",
        "snapshot.assert_match(render())",
        "pytest.fail('not yet')",
    ] {
        let src = format!("def test_x(self, save, snapshot):\n    {body}\n");
        assert!(one(&src).is_empty(), "{body}");
    }
}

#[test]
fn skipped_and_expected_failures_are_not_read() {
    for decorator in [
        "@pytest.mark.skip(reason='later')",
        "@pytest.mark.skipif(True, reason='x')",
        "@pytest.mark.xfail",
        "@unittest.skip('x')",
        "@unittest.expectedFailure",
    ] {
        let src = format!("import pytest, unittest\n\n{decorator}\ndef test_x():\n    run()\n");
        assert!(one(&src).is_empty(), "{decorator}");
    }
}

#[test]
fn an_unknown_decorator_may_assert() {
    let src = "from .checks import no_leaks\n\n@no_leaks\ndef test_x():\n    run()\n";
    assert!(one(src).is_empty());
    let on_class = "@expect_no_warnings\nclass TestX:\n    def test_x(self):\n        run()\n";
    assert!(one(on_class).is_empty());
}

#[test]
fn a_fixture_whose_name_checks_may_assert() {
    for param in ["snapshot", "django_assert_num_queries", "check_logs"] {
        let src = format!("def test_x({param}):\n    run()\n");
        assert!(one(&src).is_empty(), "{param}");
    }
}

#[test]
fn an_asserting_autouse_fixture_covers_every_test_below_it() {
    let test = ("tests/unit/test_a.py", "def test_x():\n    run()\n");
    let asserting = "import pytest\n\n@pytest.fixture(autouse=True)\ndef no_leaks():\n    yield\n    assert not LEAKS\n";
    for conftest in ["conftest.py", "tests/conftest.py", "tests/unit/conftest.py"] {
        assert!(run(&[(conftest, asserting), test]).is_empty(), "{conftest}");
    }
    let elsewhere = ("other/conftest.py", asserting);
    assert_eq!(run(&[elsewhere, test]).len(), 1);
    let setup_only = "import pytest\n\n@pytest.fixture(autouse=True)\ndef env(monkeypatch):\n    monkeypatch.delenv('X', raising=False)\n";
    assert_eq!(run(&[("tests/conftest.py", setup_only), test]).len(), 1);
    let in_module = format!("{asserting}\n\ndef test_x():\n    run()\n");
    assert!(one(&in_module).is_empty());
}

#[test]
fn helpers_that_assert_or_that_cannot_be_seen_are_assertions() {
    let src = "\
from .helpers import run_case
import tests.utils as u
from myapp import service


def confirm(x):
    assert x


def test_a():
    confirm(service.run())


def test_b():
    run_case(1)


def test_c():
    u.go()


class TestX(Base):
    def test_d(self):
        self.go_through_base()

    def _local(self):
        return 1

    def test_e(self):
        self._local()


def test_f():
    service.run()
";
    assert_eq!(one(src), vec!["test_e", "test_f"]);
}

#[test]
fn a_requested_fixture_that_asserts_on_teardown_covers_the_test() {
    let conftest = "\
import pytest


@pytest.fixture
def app():
    a = make()
    yield a
    assert not a.leaks


@pytest.fixture
def client(app):
    return app.client()


@pytest.fixture
def plain():
    return 1
";
    let tests = "\
def test_a(app):
    run()


def test_b(client):
    run()


def test_c(plain):
    run()
";
    let vs = run(&[("tests/conftest.py", conftest), ("tests/test_x.py", tests)]);
    let names: Vec<_> = vs.iter().filter_map(|v| v.object.as_deref()).collect();
    assert_eq!(names, vec!["test_c"]);
}
