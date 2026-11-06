use super::run;
use crate::arch_checker::ArchViolation;

const MANIFEST: (&str, &str) = (
    "package.json",
    r#"{ "name": "web", "private": true, "dependencies": { "react": "18.2.0", "@mui/material": "5.0.0" } }"#,
);

const BUTTON: &str = "\
interface ButtonProps {
  label: string;
  size?: 'sm' | 'md';
  onClick?: () => void;
}

export function Button({ label, size, onClick }: ButtonProps) {
  return <button className={size ?? 'md'} onClick={onClick}>{label}</button>;
}
";

const HOME: &str = "\
import { Button } from '../components/Button';
export function Home() {
  return <Button label=\"Go\" size=\"sm\" onClick={() => go()} />;
}
";

const ABOUT: &str = "\
import { Button } from '../components/Button';
const handle = () => {};
export function About() {
  return (
    <div>
      <Button label=\"Back\" size=\"md\" onClick={handle}>
      </Button>
    </div>
  );
}
";

/// The manifest, the component and two callers, plus `extra`.
fn repo(button: &str, home: &str, about: &str, extra: &[(&str, &str)]) -> Vec<ArchViolation> {
    let mut files = vec![
        MANIFEST,
        ("src/components/Button.tsx", button),
        ("src/pages/Home.tsx", home),
        ("src/pages/About.tsx", about),
    ];
    files.extend_from_slice(extra);
    run(&files)
}

fn base(extra: &[(&str, &str)]) -> Vec<ArchViolation> {
    repo(BUTTON, HOME, ABOUT, extra)
}

// ── positives ──────────────────────────────────────────────────────────────

#[test]
fn optional_props_every_caller_passes_are_flagged_once_per_component() {
    let vs = base(&[]);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    let v = &vs[0];
    assert_eq!(v.file, "src/components/Button.tsx");
    assert_eq!(v.line, 3);
    assert_eq!(v.object.as_deref(), Some("Button"));
    assert!(
        v.message
            .starts_with("`size` and `onClick` are declared optional in the props of `Button`"),
        "{}",
        v.message
    );
    assert!(
        v.message
            .contains("every one of the 2 places that render `Button` passes them with a value: src/pages/About.tsx:6 and src/pages/Home.tsx:3"),
        "{}",
        v.message
    );
}

#[test]
fn callers_through_a_barrel_an_alias_and_the_home_file_are_all_counted() {
    let barrel = (
        "src/components/index.ts",
        "export { Button } from './Button';\n",
    );
    let home = HOME
        .replace(
            "import { Button } from '../components/Button';",
            "import { Button as Btn } from '../components';",
        )
        .replace("<Button ", "<Btn ");
    let button = format!(
        "{BUTTON}export function Toolbar() {{\n  return <Button label=\"x\" size=\"sm\" onClick={{() => 1}} />;\n}}\n"
    );
    let vs = repo(&button, &home, ABOUT, &[barrel]);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert!(vs[0].message.contains("3 places"), "{}", vs[0].message);
}

#[test]
fn a_default_export_and_an_inline_props_type_are_read() {
    let button = "\
export default function Badge(props: { tone?: string; count: number }) {
  return <span className={props.tone}>{props.count}</span>;
}
";
    let a =
        "import Badge from './Badge';\nexport const A = () => <Badge tone=\"red\" count={1} />;\n";
    let b = "import Pill from './Badge';\nexport const B = () => <Pill tone={`x`} count={2} />;\n";
    let vs = run(&[
        MANIFEST,
        ("src/Badge.tsx", button),
        ("src/A.tsx", a),
        ("src/B.tsx", b),
    ]);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert!(
        vs[0].message.starts_with("`tone` is declared optional"),
        "{}",
        vs[0].message
    );
}

#[test]
fn a_type_alias_props_type_and_an_arrow_component_are_read() {
    let button = BUTTON
        .replace("interface ButtonProps {", "type ButtonProps = {")
        .replace("  onClick?: () => void;\n}", "  onClick?: () => void;\n};")
        .replace(
            "export function Button({ label, size, onClick }: ButtonProps) {",
            "export const Button = ({ label, size, onClick }: ButtonProps) => {",
        )
        .replace("</button>;\n}", "</button>;\n};");
    assert_eq!(repo(&button, HOME, ABOUT, &[]).len(), 1);
}

#[test]
fn a_bare_boolean_attribute_and_a_test_caller_that_passes_it_count() {
    let button = BUTTON.replace(
        "  size?: 'sm' | 'md';\n",
        "  size?: 'sm' | 'md';\n  wide?: boolean;\n",
    );
    let home = HOME.replace("size=\"sm\"", "size=\"sm\" wide");
    let about = ABOUT.replace("size=\"md\"", "size=\"md\" wide={false}");
    let test = (
        "src/pages/Button.test.tsx",
        "import { Button } from '../components/Button';\ntest('x', () => { render(<Button label=\"t\" size=\"sm\" wide onClick={() => 1} />); });\n",
    );
    let vs = repo(&button, &home, &about, &[test]);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert!(
        vs[0].message.contains("`size`, `wide` and `onClick`"),
        "{}",
        vs[0].message
    );
}

// ── safe twins and traps ───────────────────────────────────────────────────

#[test]
fn safe_twin_an_optional_prop_one_caller_omits_is_needed() {
    let about = ABOUT.replace(" size=\"md\"", "");
    let vs = repo(BUTTON, HOME, &about, &[]);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert!(
        vs[0].message.starts_with("`onClick` is declared optional"),
        "{}",
        vs[0].message
    );
    let both = about.replace(" onClick={handle}", "");
    assert!(repo(BUTTON, HOME, &both, &[]).is_empty());
}

#[test]
fn a_test_or_story_caller_that_omits_the_prop_keeps_it_optional() {
    let test = (
        "src/pages/Button.test.tsx",
        "import { Button } from '../components/Button';\nit('renders', () => render(<Button label=\"t\" />));\n",
    );
    assert!(base(&[test]).is_empty());
}

#[test]
fn a_library_package_or_a_missing_manifest_is_never_read() {
    for manifest in [
        r#"{ "name": "ui", "main": "dist/index.js" }"#,
        r#"{ "name": "ui", "exports": { ".": "./src/index.ts" } }"#,
        r#"{ "name": "ui", "types": "dist/index.d.ts" }"#,
    ] {
        let vs = run(&[
            ("package.json", manifest),
            ("src/components/Button.tsx", BUTTON),
            ("src/pages/Home.tsx", HOME),
            ("src/pages/About.tsx", ABOUT),
        ]);
        assert!(vs.is_empty(), "{manifest}: {vs:#?}");
    }
    let vs = run(&[
        ("src/components/Button.tsx", BUTTON),
        ("src/pages/Home.tsx", HOME),
        ("src/pages/About.tsx", ABOUT),
    ]);
    assert!(vs.is_empty());
}

#[test]
fn a_spread_at_any_call_site_makes_the_component_unreadable() {
    let about = ABOUT.replace("onClick={handle}", "onClick={handle} {...rest}");
    assert!(repo(BUTTON, HOME, &about, &[]).is_empty());
}

#[test]
fn a_value_that_may_be_undefined_does_not_count_as_passing() {
    for value in [
        "size={undefined}",
        "size={props.size}",
        "size={pick()}",
        "size={flag ? 'sm' : undefined}",
        "size={user?.size}",
        "size={unknownName}",
    ] {
        let about = ABOUT.replace("size=\"md\"", value);
        let vs = repo(BUTTON, HOME, &about, &[]);
        assert_eq!(vs.len(), 1, "{value}: {vs:#?}");
        assert!(
            vs[0].message.starts_with("`onClick` is declared optional"),
            "{value}: {}",
            vs[0].message
        );
    }
}

#[test]
fn a_name_bound_twice_in_the_caller_is_not_a_known_constant() {
    let about = ABOUT.replace(
        "export function About() {",
        "export function About({ handle }: { handle?: () => void }) {",
    );
    let vs = repo(BUTTON, HOME, &about, &[]);
    assert_eq!(vs.len(), 1, "{vs:#?}");
    assert!(
        vs[0].message.starts_with("`size` is declared optional"),
        "{}",
        vs[0].message
    );
}

#[test]
fn a_value_use_of_the_component_anywhere_makes_its_callers_unknown() {
    for (path, src) in [
        ("src/memo.tsx", "import { Button } from './components/Button';\nexport const M = memo(Button);\n"),
        ("src/Button.stories.tsx", "import { Button } from './components/Button';\nexport default { component: Button };\n"),
        ("src/props.ts", "import { Button } from './components/Button';\nexport type P = React.ComponentProps<typeof Button>;\n"),
        ("src/el.ts", "import { Button } from './components/Button';\nexport const e = createElement(Button, { label: 'x' });\n"),
    ] {
        assert!(base(&[(path, src)]).is_empty(), "{path}");
    }
}

#[test]
fn a_namespace_import_or_a_dynamic_import_makes_its_callers_unknown() {
    for (path, src) in [
        ("src/ns.tsx", "import * as C from './components/Button';\nexport const N = () => <C.Button label=\"x\" />;\n"),
        ("src/lazy.tsx", "const L = lazy(() => import('./components/Button'));\n"),
        ("src/req.js", "const b = require('./components/Button');\n"),
        ("src/barrel-ns.tsx", "import * as UI from './components';\nexport const N = () => <UI.Button label=\"x\" />;\n"),
    ] {
        let barrel = ("src/components/index.ts", "export * from './Button';\n");
        assert!(base(&[barrel, (path, src)]).is_empty(), "{path}");
    }
}

#[test]
fn an_unrelated_namespace_import_does_not_hide_the_finding() {
    let other = (
        "src/util.ts",
        "import * as path from 'node:path';\nimport * as h from './helpers';\nexport const u = 1;\n",
    );
    let helpers = ("src/helpers.ts", "export const x = 1;\n");
    assert_eq!(base(&[other, helpers]).len(), 1);
}

#[test]
fn an_import_the_resolver_cannot_follow_could_be_a_caller() {
    for src in [
        "import { Button } from '@/components/Button';\nexport const X = () => <Button label=\"x\" />;\n",
        "import { Button as B } from '~/components/Button';\nexport const X = () => <B label=\"x\" />;\n",
    ] {
        assert!(base(&[("src/x.tsx", src)]).is_empty(), "{src}");
    }
    // A default import from an unresolved alias could be any default-exported component.
    let badge = "export default function Badge(props: { tone?: string }) {\n  return <i>{props.tone}</i>;\n}\n";
    let vs = run(&[
        MANIFEST,
        ("src/Badge.tsx", badge),
        (
            "src/A.tsx",
            "import Badge from './Badge';\nexport const A = () => <Badge tone=\"a\" />;\n",
        ),
        (
            "src/B.tsx",
            "import Badge from './Badge';\nexport const B = () => <Badge tone=\"b\" />;\n",
        ),
        (
            "src/C.tsx",
            "import Thing from '@/Badge';\nexport const C = () => <Thing />;\n",
        ),
    ]);
    assert!(vs.is_empty(), "{vs:#?}");
}

#[test]
fn a_same_named_component_from_a_dependency_is_a_different_symbol() {
    let other = (
        "src/Other.tsx",
        "import { Button } from '@mui/material';\nexport const O = () => <Button />;\n",
    );
    assert_eq!(base(&[other]).len(), 1);
}

#[test]
fn a_file_naming_the_component_without_an_import_could_be_a_caller() {
    let global = (
        "src/global.tsx",
        "export const G = () => <Button label=\"x\" />;\n",
    );
    assert!(base(&[global]).is_empty());
    let own = (
        "src/own.tsx",
        "function Button() { return <b />; }\nexport const G = () => <Button />;\n",
    );
    assert_eq!(base(&[own]).len(), 1);
}

#[test]
fn template_files_and_skipped_code_naming_the_component_make_its_callers_unknown() {
    for (path, src) in [
        ("docs/Button.mdx", "<Button label=\"x\" />\n"),
        ("src/App.vue", "<template><Button /></template>\n"),
        (
            "src/generated/routes.ts",
            "import { Button } from '../components/Button';\nButton;\n",
        ),
    ] {
        assert!(base(&[(path, src)]).is_empty(), "{path}");
    }
}

#[test]
fn a_computed_load_that_can_reach_the_component_makes_its_callers_unknown() {
    let glob = (
        "src/registry.ts",
        "export const all = import.meta.glob('./components/*.tsx');\n",
    );
    assert!(base(&[glob]).is_empty());
    let elsewhere = (
        "src/registry.ts",
        "export const all = import.meta.glob('./icons/*.svg');\n",
    );
    assert_eq!(base(&[elsewhere]).len(), 1);
}

#[test]
fn a_shared_exported_or_generic_props_type_is_not_read() {
    for button in [
        BUTTON.replace("interface ButtonProps {", "export interface ButtonProps {"),
        format!("{BUTTON}export const defaults: ButtonProps = {{ label: 'x' }};\n"),
        BUTTON
            .replace("export function Button(", "export function Button<T>(")
            .replace(": ButtonProps)", ": ButtonProps & T)"),
        BUTTON.replace(": ButtonProps)", ": ButtonProps | Other)"),
    ] {
        assert!(repo(&button, HOME, ABOUT, &[]).is_empty(), "{button}");
    }
}

#[test]
fn a_destructuring_default_is_a_documented_choice() {
    let button = BUTTON.replace(
        "{ label, size, onClick }",
        "{ label, size = 'md', onClick = noop }",
    );
    assert!(repo(&button, HOME, ABOUT, &[]).is_empty());
}

#[test]
fn children_is_never_reported() {
    let button = BUTTON.replace(
        "  onClick?: () => void;\n",
        "  onClick?: () => void;\n  children?: React.ReactNode;\n",
    );
    let home = HOME.replace(
        " onClick={() => go()} />",
        " onClick={() => go()} children={<i />} />",
    );
    let about = ABOUT.replace(" onClick={handle}", " onClick={handle} children=\"x\"");
    let vs = repo(&button, &home, &about, &[]);
    assert_eq!(vs.len(), 1);
    assert!(!vs[0].message.contains("`children`"), "{}", vs[0].message);
}

#[test]
fn a_single_call_site_is_not_enough() {
    let vs = run(&[
        MANIFEST,
        ("src/components/Button.tsx", BUTTON),
        ("src/pages/Home.tsx", HOME),
    ]);
    assert!(vs.is_empty());
}

#[test]
fn javascript_components_have_no_optional_props_to_read() {
    let vs = run(&[
        MANIFEST,
        ("src/components/Button.jsx", "export function Button({ size }) { return <b>{size}</b>; }\n"),
        ("src/pages/Home.jsx", "import { Button } from '../components/Button';\nexport const H = () => <Button size=\"a\" />;\n"),
        ("src/pages/About.jsx", "import { Button } from '../components/Button';\nexport const A = () => <Button size=\"b\" />;\n"),
    ]);
    assert!(vs.is_empty());
}

#[test]
fn a_component_a_framework_router_renders_has_a_caller_no_import_shows() {
    let page = "\
export default function Page(props: { title?: string }) {
  return <h1>{props.title}</h1>;
}
";
    let a =
        "import Page from '../app/orders/page';\nexport const A = () => <Page title=\"a\" />;\n";
    let b =
        "import Page from '../app/orders/page';\nexport const B = () => <Page title=\"b\" />;\n";
    let files = |manifest: &'static str| {
        run(&[
            ("package.json", manifest),
            ("app/orders/page.tsx", page),
            ("tests/A.tsx", a),
            ("tests/B.tsx", b),
        ])
    };
    assert!(files(
        r#"{ "private": true, "dependencies": { "next": "14.0.0", "react": "18.2.0" } }"#
    )
    .is_empty());
    // The same folder layout without a file-system router is ordinary code.
    assert_eq!(
        files(r#"{ "private": true, "dependencies": { "react": "18.2.0" } }"#).len(),
        1
    );
}
