#!/usr/bin/env python3
"""Build the client-side search index from the rendered site.

Every docs page is hand-authored HTML inside its markdown body, so Zola's own
search index — which only ever sees the markdown source — would index the raw
tags instead of the prose. Reading `public/` after the build sidesteps that: the
rendered page is the single source of truth for both the prose and the heading
ids the dialog links to.

One record per page:

    title     the page `<h1>`, falling back to the document `<title>`
    crumb     the parent section's title, or "Docs" for a top-level page
    url       site-relative, e.g. "resilience/circuit-breakers/" ("" is home)
    headings   [{"id", "text"}] for every `<h2>`/`<h3>` carrying an id
    text      visible prose, whitespace-collapsed and truncated

Usage:  python3 search-index.py [public_dir]
Writes: <public_dir>/search-index.json
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from html.parser import HTMLParser
from pathlib import Path

SPACE = re.compile(r"\s+")

# A page is only worth finding if any phrase on it matches, so the cap is set
# to bound a runaway page rather than to trim the normal ones: the pages run
# 2-12 KB of prose (median ~5 KB), and this leaves 24 of 28 whole. The index
# lands around 165 KB, fetched once when the dialog first opens.
TEXT_LIMIT = 8000

# Root-level pages have no parent section to name.
ROOT_CRUMB = "Docs"


class Page(HTMLParser):
    """One linear pass over a rendered page: title, headings, prose.

    Everything that is not prose is dropped as a whole subtree:

    * `svg` — the diagrams are inline, and a single one carries hundreds of
      path-coordinate tokens plus a `<title>` caption that would swamp both the
      document title and the page text.
    * `nav` — `nav.page-toc` repeats every heading on the page verbatim.
    * `pre` — fenced code blocks. Inline `<code>` is deliberately *kept*: the
      API names in running prose (`TaskResult`, `CheckpointStore`) are the most
      searched terms on the site.
    * `a.anchor-link` — the `#` glyph Zola puts inside every heading.
    """

    SKIP_TAGS = {"script", "style", "svg", "button", "noscript", "nav", "pre", "template"}
    HEADINGS = {"h1", "h2", "h3"}

    # Adjacent list items and table cells carry no whitespace between them, so
    # their text would otherwise concatenate into one unsearchable word.
    BLOCK = {
        "p", "div", "br", "li", "ul", "ol", "dl", "dt", "dd", "pre", "table",
        "tr", "td", "th", "thead", "tbody", "section", "article", "figure",
        "blockquote", "h1", "h2", "h3", "h4", "h5", "h6", "hr",
    }

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.doc_title = ""
        self.h1 = ""
        self.headings: list[dict[str, str]] = []
        self.seen_main = False
        self._text: list[str] = []
        self._in_title = False
        self._main = 0
        # `svg` is tracked outside `main` too, so a decorative icon's `<title>`
        # in the header can never be mistaken for the document title.
        self._svg = 0
        self._skip = 0
        self._skip_tag = ""
        self._heading: list[str] | None = None
        self._heading_tag = ""
        self._heading_id = ""

    # -- structure ---------------------------------------------------------

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if self._svg:
            if tag == "svg":
                self._svg += 1
            return
        if tag == "svg":
            self._svg = 1
            return

        if self._main == 0:
            if tag == "title":
                self._in_title = True
            elif tag == "main":
                self._main = 1
                self.seen_main = True
            return

        if self._skip:
            # Only the tag that opened the region can nest it deeper, so void
            # elements inside it (`<path/>`, `<br>`) cannot unbalance the count.
            if tag == self._skip_tag:
                self._skip += 1
            return

        attr = dict(attrs)
        if tag in self.SKIP_TAGS or (tag == "a" and "anchor-link" in (attr.get("class") or "")):
            self._skip, self._skip_tag = 1, tag
            return

        if tag == "main":
            self._main += 1
        elif tag in self.HEADINGS and self._heading is None:
            self._heading = []
            self._heading_tag = tag
            self._heading_id = attr.get("id") or ""
        elif tag in self.BLOCK:
            self._text.append(" ")

    def handle_endtag(self, tag: str) -> None:
        if self._svg:
            if tag == "svg":
                self._svg -= 1
            return

        if self._main == 0:
            if tag == "title":
                self._in_title = False
            return

        if self._skip:
            if tag == self._skip_tag:
                self._skip -= 1
            return

        if tag == "main":
            self._main -= 1
        elif self._heading is not None and tag == self._heading_tag:
            self._close_heading(tag)
        elif tag in self.BLOCK:
            self._text.append(" ")

    def handle_data(self, data: str) -> None:
        if self._svg or self._skip:
            return
        if self._in_title:
            self.doc_title += data
        elif self._main:
            if self._heading is not None:
                self._heading.append(data)
            else:
                self._text.append(data)

    # -- collection --------------------------------------------------------

    def _close_heading(self, tag: str) -> None:
        text = collapse("".join(self._heading or ()))
        self._heading = None
        if not text:
            return
        # Headings stay in the prose stream as well: a page's section titles
        # are the terms a reader is most likely to type.
        self._text.append(f" {text} ")
        if tag == "h1":
            if not self.h1:
                self.h1 = text
        elif self._heading_id:
            self.headings.append({"id": self._heading_id, "text": text})

    @property
    def text(self) -> str:
        return collapse("".join(self._text))


def collapse(text: str) -> str:
    return SPACE.sub(" ", text).strip()


def truncate(text: str, limit: int = TEXT_LIMIT) -> str:
    """Cut to `limit`, backing up to the last word boundary."""
    if len(text) <= limit:
        return text
    cut = text[:limit]
    space = cut.rfind(" ")
    return (cut[:space] if space > limit // 2 else cut).rstrip()


def prettify(slug: str) -> str:
    return slug.replace("-", " ").replace("_", " ").title()


def page_record(html: str, url: str) -> dict | None:
    page = Page()
    page.feed(html)
    page.close()

    # No `<h1>` means no page: the 404 stub, or a template that renders nothing.
    if not page.seen_main or not page.h1:
        return None

    return {
        "title": page.h1,
        # Filled in once every page's title is known.
        "crumb": "",
        "url": url,
        "headings": page.headings,
        "text": truncate(page.text),
    }


def build(public: Path) -> list[dict]:
    records = []
    for html_file in sorted(public.rglob("*.html")):
        if html_file.name == "404.html":
            continue
        rel = html_file.relative_to(public).parent.as_posix()
        url = "" if rel == "." else rel + "/"
        record = page_record(html_file.read_text(encoding="utf-8"), url)
        if record is not None:
            records.append(record)

    # The breadcrumb is the parent section's own heading, so "Circuit Breakers"
    # reads under "Resilience" without restating the nav tree in this script.
    titles = {record["url"]: record["title"] for record in records}
    for record in records:
        parts = record["url"].strip("/").split("/") if record["url"] else []
        if len(parts) < 2:
            record["crumb"] = ROOT_CRUMB
        else:
            parent = "/".join(parts[:-1]) + "/"
            record["crumb"] = titles.get(parent) or prettify(parts[-2])

    records.sort(key=lambda record: record["url"])
    return records


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "public",
        nargs="?",
        default=Path(__file__).resolve().parent / "public",
        type=Path,
        help="the rendered site (default: the public/ next to this script)",
    )
    args = parser.parse_args()

    if not args.public.is_dir():
        print(f"no such directory: {args.public}", file=sys.stderr)
        print("run `zola build` first", file=sys.stderr)
        return 1

    records = build(args.public)
    if not records:
        print(f"no pages found under {args.public}", file=sys.stderr)
        return 1

    out = args.public / "search-index.json"
    out.write_text(
        json.dumps(records, separators=(",", ":"), ensure_ascii=False),
        encoding="utf-8",
    )
    headings = sum(len(record["headings"]) for record in records)
    print(f"indexed {len(records)} pages, {headings} headings into {out} ({out.stat().st_size // 1024} KB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
