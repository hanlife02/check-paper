#!/usr/bin/env python3
"""Import PDFs into check-paper's paper/<AUTHOR>/<paper-id>/article.md layout."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


DOI_RE = re.compile(r"\b10\.\d{4,9}/[-._;()/:A-Z0-9]+\b", re.IGNORECASE)
YEAR_RE = re.compile(r"\b(?:19|20)\d{2}\b")


@dataclass
class PdfExtract:
    text: str
    title: str
    warnings: list[str]


def main() -> int:
    args = parse_args()
    project_root = Path(args.project_root).expanduser().resolve()
    paper_root = Path(args.paper_root).expanduser().resolve() if args.paper_root else project_root / "paper"
    if not project_root.exists():
        fail(f"project root does not exist: {project_root}")
    if not args.author.strip():
        fail("--author must not be empty")

    pdfs = resolve_pdfs(args.pdfs)
    if not pdfs:
        fail("no PDF files found")

    created = []
    for pdf in pdfs:
        extract = extract_pdf(pdf)
        if len(extract.text.strip()) < args.min_text_chars:
            extract.warnings.append(
                f"extracted text is short ({len(extract.text.strip())} chars); PDF may need OCR"
            )
        metadata = infer_metadata(pdf, extract)
        paper_id = unique_paper_id(paper_root / args.author, metadata)
        out_dir = paper_root / args.author / paper_id
        if args.dry_run:
            print(f"would import {pdf} -> {out_dir}")
            continue
        out_dir.mkdir(parents=True, exist_ok=False)
        shutil.copy2(pdf, out_dir / "source.pdf")
        article = render_article(metadata, extract.text)
        (out_dir / "article.md").write_text(article, encoding="utf-8")
        fetch_result = {
            "source": "pdf_import",
            "access_method": "user_selected_pdf",
            "doi": metadata["doi"],
            "title": metadata["title"],
            "year": metadata["year"],
            "status": "pdf_imported",
            "has_fulltext": True,
            "text_chars": len(extract.text),
            "source_pdf": "source.pdf",
            "export_warnings": extract.warnings,
        }
        (out_dir / "fetch-result.json").write_text(
            json.dumps(fetch_result, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
        created.append(out_dir)
        print(f"imported: {pdf} -> {out_dir}")
        for warning in extract.warnings:
            print(f"  warning: {warning}")

    if created:
        print("\nnext:")
        print(f"  target/debug/ppc scan --author {json.dumps(args.author)}")
        print(f"  target/debug/ppc ingest --author {json.dumps(args.author)}")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Import selected PDFs into check-paper paper/<AUTHOR>/<paper-id>/ format."
    )
    parser.add_argument("pdfs", nargs="+", help="PDF file paths or directories containing PDFs")
    parser.add_argument("--project-root", default=".", help="check-paper project root")
    parser.add_argument("--paper-root", help="paper root; default: <project-root>/paper")
    parser.add_argument("--author", required=True, help="target author directory name")
    parser.add_argument(
        "--min-text-chars",
        type=int,
        default=1200,
        help="warn when extracted text is shorter than this value",
    )
    parser.add_argument("--dry-run", action="store_true", help="show target paths without writing")
    return parser.parse_args()


def resolve_pdfs(inputs: list[str]) -> list[Path]:
    pdfs: list[Path] = []
    for item in inputs:
        path = Path(item).expanduser().resolve()
        if path.is_dir():
            pdfs.extend(sorted(path.rglob("*.pdf")))
        elif path.is_file() and path.suffix.lower() == ".pdf":
            pdfs.append(path)
        else:
            fail(f"not a PDF file or directory: {path}")
    seen = set()
    result = []
    for pdf in pdfs:
        if pdf not in seen:
            seen.add(pdf)
            result.append(pdf)
    return result


def extract_pdf(pdf: Path) -> PdfExtract:
    warnings: list[str] = []
    for extractor in (extract_with_pypdf, extract_with_pypdf2, extract_with_pdftotext):
        try:
            result = extractor(pdf)
        except Exception as error:  # noqa: BLE001 - keep optional extractor fallbacks simple
            warnings.append(f"{extractor.__name__} failed: {error}")
            continue
        if result.text.strip():
            result.warnings = warnings + result.warnings
            return result
        warnings.append(f"{extractor.__name__} produced no text")
    fail(
        "could not extract PDF text. Install pypdf/PyPDF2 or pdftotext, or run OCR first: "
        f"{pdf}"
    )


def extract_with_pypdf(pdf: Path) -> PdfExtract:
    from pypdf import PdfReader  # type: ignore

    reader = PdfReader(str(pdf))
    title = str(reader.metadata.title or "") if reader.metadata else ""
    text = "\n\n".join((page.extract_text() or "") for page in reader.pages)
    return PdfExtract(normalize_text(text), clean_metadata_title(title), [])


def extract_with_pypdf2(pdf: Path) -> PdfExtract:
    from PyPDF2 import PdfReader  # type: ignore

    reader = PdfReader(str(pdf))
    title = ""
    metadata = getattr(reader, "metadata", None) or getattr(reader, "documentInfo", None)
    if metadata:
        title = str(getattr(metadata, "title", "") or metadata.get("/Title", "") or "")
    text = "\n\n".join((page.extract_text() or "") for page in reader.pages)
    return PdfExtract(normalize_text(text), clean_metadata_title(title), [])


def extract_with_pdftotext(pdf: Path) -> PdfExtract:
    completed = subprocess.run(
        ["pdftotext", "-layout", str(pdf), "-"],
        check=True,
        capture_output=True,
        text=True,
    )
    return PdfExtract(normalize_text(completed.stdout), "", [])


def infer_metadata(pdf: Path, extract: PdfExtract) -> dict[str, str]:
    text = extract.text
    doi = find_doi(text)
    year = find_year(text) or find_year(f"{pdf.parent.name} {pdf.stem}")
    title = extract.title or find_title(text) or title_from_filename(pdf)
    return {
        "title": title,
        "doi": doi,
        "year": year,
    }


def find_doi(text: str) -> str:
    match = DOI_RE.search(text)
    if not match:
        return ""
    return match.group(0).rstrip(".,;)]}").strip()


def find_year(text: str) -> str:
    years = YEAR_RE.findall(text[:8000])
    if not years:
        return ""
    match = YEAR_RE.search(text[:8000])
    return match.group(0) if match else ""


def find_title(text: str) -> str:
    lines = [clean_line(line) for line in text.splitlines()[:120]]
    lines = [line for line in lines if line]
    candidates = []
    for index, line in enumerate(lines):
        if not plausible_title(line):
            continue
        combined = line
        for next_line in lines[index + 1 : index + 3]:
            if not plausible_title_continuation(next_line):
                break
            combined = f"{combined} {next_line}"
        candidates.append(combined)
    if not candidates:
        return ""
    return candidates[0]


def plausible_title(line: str) -> bool:
    if len(line) < 20 or len(line) > 240:
        return False
    lower = line.lower()
    blocked = [
        "abstract",
        "electronic supplementary material",
        "keywords",
        "introduction",
        "copyright",
        "downloaded",
        "journal",
        "volume",
        "page ",
        "doi:",
        "http://",
        "https://",
        "university",
        "laboratory",
        "college",
        "department",
        "institute",
        "correspondence",
        "received:",
        "accepted:",
        "published",
    ]
    return not any(token in lower for token in blocked)


def plausible_title_continuation(line: str) -> bool:
    if not plausible_title(line):
        return False
    lower = line.lower()
    if any(token in lower for token in ["@ ", "@ ", "author", "†", "‡", "§"]):
        return False
    if line.count(",") >= 2:
        return False
    if re.search(r"\b\d{4,6},\s*[A-Z][a-z]+", line):
        return False
    return True


def title_from_filename(pdf: Path) -> str:
    title = re.sub(r"[_\-]+", " ", pdf.stem)
    title = re.sub(r"\s+", " ", title).strip()
    return title or pdf.stem


def unique_paper_id(author_root: Path, metadata: dict[str, str]) -> str:
    base = paper_id(metadata)
    candidate = base
    index = 2
    while (author_root / candidate).exists():
        candidate = f"{base}-{index}"
        index += 1
    return candidate


def paper_id(metadata: dict[str, str]) -> str:
    year = metadata["year"] or "undated"
    source = metadata["doi"] or metadata["title"]
    slug = slugify(source) or "paper"
    return f"{year}-{slug}"


def slugify(value: str) -> str:
    value = value.lower().strip()
    value = value.replace("/", "-")
    value = re.sub(r"[^a-z0-9]+", "-", value)
    value = re.sub(r"-+", "-", value).strip("-")
    return value[:96].strip("-")


def render_article(metadata: dict[str, str], body: str) -> str:
    title = metadata["title"]
    doi = metadata["doi"]
    year = metadata["year"]
    frontmatter = {
        "title": title,
        "doi": doi,
        "year": year,
        "source": "pdf_import",
        "has_fulltext": "true",
        "access_method": "user_selected_pdf",
        "text_chars": str(len(body)),
    }
    lines = ["---"]
    for key, value in frontmatter.items():
        lines.append(f'{key}: "{escape_yaml(value)}"')
    lines.extend(["---", "", f"# {title}", ""])
    if doi:
        lines.append(f"- DOI: `{doi}`")
    if year:
        lines.append(f"- Year: `{year}`")
    lines.extend(["- Source: User-selected PDF", "", "## Article Body", "", body.strip(), ""])
    return "\n".join(lines)


def normalize_text(text: str) -> str:
    text = text.replace("\x00", "")
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = re.sub(r"[ \t]+\n", "\n", text)
    text = re.sub(r"\n{4,}", "\n\n\n", text)
    return text.strip()


def clean_line(line: str) -> str:
    return re.sub(r"\s+", " ", line).strip()


def clean_metadata_title(title: str) -> str:
    title = clean_line(title)
    if title.lower() in {"untitled", "unknown"}:
        return ""
    return title


def escape_yaml(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"')


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


if __name__ == "__main__":
    raise SystemExit(main())
