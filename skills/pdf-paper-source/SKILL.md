---
name: pdf-paper-source
description: "Convert user-selected PDF literature into check-paper paper source directories. Trigger when the user asks to import, organize, convert, or prepare PDFs as paper author and paper-id article.md data sources for ppc scan, ingest, or sync."
---

# PDF Paper Source

## Target Format

Create one directory per paper:

```text
paper/<AUTHOR>/<PAPER_ID>/
  article.md
  fetch-result.json
  source.pdf
```

`article.md` is required. Preserve the original PDF as `source.pdf`; write `fetch-result.json` when metadata is available.

## Import Workflow

1. Resolve PDF inputs from user-provided paths, attached files, or directories. Do not invent missing papers.
2. Resolve the target author. If omitted, inspect:

```bash
target/debug/ppc authors
target/debug/ppc config --show
```

3. Convert PDFs with the bundled helper:

```bash
python3 skills/pdf-paper-source/scripts/import_pdfs.py \
  --project-root /Users/hanlife02/code/check-paper \
  --author "AUTHOR" \
  /path/to/paper.pdf
```

If the helper cannot extract text, manually create the same layout using `pdftotext`, `pypdf`, `PyPDF2`, or OCR. Keep extracted text faithful; do not summarize the PDF into `article.md`.

4. Inspect generated `article.md` frontmatter and first page text. Fix obvious title/year/DOI mistakes before ingest.
5. Validate:

```bash
target/debug/ppc scan --author "AUTHOR"
target/debug/ppc ingest --author "AUTHOR"
```

Use `target/debug/ppc sync --author "AUTHOR"` only when the user wants ingest plus analysis queueing.

## article.md Rules

Use YAML frontmatter:

```markdown
---
title: "Paper title"
doi: "10.xxxx/xxxxx"
year: "2024"
source: "pdf_import"
has_fulltext: true
access_method: "user_selected_pdf"
text_chars: 12345
---

# Paper title

- DOI: `10.xxxx/xxxxx`
- Year: `2024`
- Source: User-selected PDF

## Article Body

Full extracted text here.
```

For scanned PDFs or very short extracted text, report that OCR is needed instead of creating low-quality source text silently.

## Naming

Use deterministic ASCII slugs:

- DOI present: `<year>-<normalized-doi>`
- DOI missing: `<year-or-undated>-<title-or-filename-slug>`

Never overwrite an existing paper directory without explicit user approval. Add a numeric suffix if importing a distinct file with the same slug.

## After Import

After successful ingest, the user can ask:

```bash
target/debug/ppc ask --author "AUTHOR" "这几篇论文的主要贡献是什么？"
```

For LLM analysis:

```bash
target/debug/ppc analyze --author "AUTHOR" --limit 5
```
