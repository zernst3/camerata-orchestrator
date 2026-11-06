# Report fonts

The PDF report embeds these fonts (W27, `docs/design/2026-10-27_pdf-brand-guide.md`). They are
compiled into the server binary (`report_export::TEMPLATE_ASSETS`) and passed to Typst with
`--font-path`, which Typst searches before system fonts.

| File | Family | Source |
|---|---|---|
| `Inter-Regular.ttf`, `Inter-Italic.ttf`, `Inter-SemiBold.ttf`, `Inter-SemiBoldItalic.ttf` | Inter 4.1 | `Inter-4.1.zip` from the rsms/inter GitHub release v4.1, `extras/ttf/` (zip sha256 `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e`) |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono 2.304 | `JetBrainsMono-2.304.zip` from the JetBrains/JetBrainsMono GitHub release v2.304, `fonts/ttf/` (zip sha256 `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`) |

Both are licensed under the SIL Open Font License 1.1, which allows embedding them in documents
and bundling them with software: `Inter-OFL.txt` and `JetBrainsMono-OFL.txt` are the licence
files from the same release archives.

The template uses weights 400 and 600 only (`*strong*` is mapped to 600). Adding a weight means
adding its file here and to `TEMPLATE_ASSETS`.
