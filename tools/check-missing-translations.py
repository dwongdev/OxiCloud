#!/usr/bin/env python3
"""
check-missing-translations.py — Audit locale files against en.json.

Treats static/locales/en.json as the canonical key set. For every other
JSON file in static/locales/, reports keys that are missing (present in
en.json, absent here) and optionally keys that are extra (present here,
absent in en.json — usually drift from a removed feature).

Exit code:
    0 — every non-English locale has every English key
    1 — at least one locale is missing one or more keys

Usage:
    python3 tools/check-missing-translations.py [options]

Options:
    --check-only          CI-friendly: print per-locale counts only
                          (no per-key list). Exit code is unchanged —
                          the script always returns 1 on any miss,
                          this flag just keeps the CI log terse.
                          Mirrors `tools/check-icons.py --check-only`.
    --no-extras           Suppress the "extra keys" section.
    --values              Show the English source value next to each
                          missing key (truncated to 80 chars).
    --locale CODE [CODE…] Audit only the listed locale(s) (e.g. fr de).
                          Default: every non-English file in the dir.

Examples:
    # Verbose audit of every locale, including extras
    python3 tools/check-missing-translations.py

    # CI mode — terse output, exit 1 on any miss
    python3 tools/check-missing-translations.py --check-only

    # Just French and Spanish, with English values to help translators
    python3 tools/check-missing-translations.py --locale fr es --values
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
LOCALES_DIR = REPO_ROOT / "static" / "locales"
SOURCE_LOCALE = "en"

# Truncation length for the English source value shown by --values.
VALUE_PREVIEW_LEN = 80


def flatten(obj: dict[str, Any], prefix: str = "") -> dict[str, Any]:
    """Walk a nested JSON object and produce a flat {"dotted.key": value}
    dict. Non-dict leaves (strings, numbers, booleans, arrays) are kept
    as-is; only nested dicts are expanded into the key path."""
    out: dict[str, Any] = {}
    for key, value in obj.items():
        full = f"{prefix}.{key}" if prefix else key
        if isinstance(value, dict):
            out.update(flatten(value, full))
        else:
            out[full] = value
    return out


def truncate(text: str, max_len: int) -> str:
    """Visual truncation for terminal output. Newlines normalised to
    spaces so a multi-line email body still fits on one row."""
    one_line = text.replace("\n", " ").replace("\r", " ")
    if len(one_line) <= max_len:
        return one_line
    return one_line[: max_len - 1] + "…"


def load_locale(path: Path) -> dict[str, Any]:
    """Parse one locale file. Returns the flattened key set."""
    try:
        with path.open("r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"✗ {path.name}: could not parse ({exc})", file=sys.stderr)
        sys.exit(1)
    if not isinstance(data, dict):
        print(f"✗ {path.name}: root must be an object", file=sys.stderr)
        sys.exit(1)
    return flatten(data)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Audit static/locales/*.json against en.json. Reports keys "
            "missing from each non-English locale and (optionally) keys "
            "that exist in non-English but not in English."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--check-only",
        action="store_true",
        help=(
            "CI mode: print per-locale counts only, not the full key "
            "list. Exit code is unchanged (1 on any miss). Mirrors "
            "`tools/check-icons.py --check-only`."
        ),
    )
    parser.add_argument(
        "--no-extras",
        action="store_true",
        help="Suppress the 'extra keys' section (default: report them).",
    )
    parser.add_argument(
        "--values",
        action="store_true",
        help=(
            "Print the English source value next to each missing key. "
            "Helpful for translators; ignored under --check-only."
        ),
    )
    parser.add_argument(
        "--locale",
        nargs="+",
        metavar="CODE",
        help=(
            "Audit only the given locale code(s) (e.g. 'fr', 'zh-TW'). "
            "Default: every *.json in static/locales/ except en.json."
        ),
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    if not LOCALES_DIR.is_dir():
        print(f"✗ Locales directory not found: {LOCALES_DIR}", file=sys.stderr)
        return 1

    source_path = LOCALES_DIR / f"{SOURCE_LOCALE}.json"
    if not source_path.exists():
        print(f"✗ Source locale not found: {source_path}", file=sys.stderr)
        return 1

    en = load_locale(source_path)
    en_keys = set(en.keys())

    # Decide which locales to audit. --locale narrows the set; otherwise
    # we audit everything except en.json.
    if args.locale:
        locale_paths = []
        for code in args.locale:
            p = LOCALES_DIR / f"{code}.json"
            if not p.exists():
                print(f"✗ Locale file not found: {p}", file=sys.stderr)
                return 1
            if code == SOURCE_LOCALE:
                print(
                    f"⚠ Skipping --locale {code}: that's the source locale.",
                    file=sys.stderr,
                )
                continue
            locale_paths.append(p)
    else:
        locale_paths = sorted(
            p
            for p in LOCALES_DIR.glob("*.json")
            if p.stem != SOURCE_LOCALE
        )

    if not locale_paths:
        print("Nothing to check.")
        return 0

    print(f"Source: {source_path.relative_to(REPO_ROOT)} ({len(en_keys)} keys)\n")

    total_missing = 0
    total_extra = 0
    locale_with_missing: list[str] = []

    for path in locale_paths:
        loc = load_locale(path)
        loc_keys = set(loc.keys())
        missing = sorted(en_keys - loc_keys)
        extra = sorted(loc_keys - en_keys)
        total_missing += len(missing)
        total_extra += len(extra)
        if missing:
            locale_with_missing.append(path.stem)

        mark = "✓" if not missing else "✗"
        suffix = ""
        if missing or extra:
            parts = []
            if missing:
                parts.append(f"missing={len(missing)}")
            if extra:
                parts.append(f"extra={len(extra)}")
            suffix = "  " + "  ".join(parts)
        print(f"  {mark}  {path.name:14}  total={len(loc_keys)}{suffix}")

        if args.check_only:
            continue

        # Per-key listing (suppressed under --check-only).
        if missing:
            print(f"     missing ({len(missing)}):")
            for key in missing:
                if args.values:
                    val = en.get(key, "")
                    if not isinstance(val, str):
                        val = json.dumps(val, ensure_ascii=False)
                    preview = truncate(val, VALUE_PREVIEW_LEN)
                    print(f"        - {key}  ::  {preview}")
                else:
                    print(f"        - {key}")
        if extra and not args.no_extras:
            print(f"     extra ({len(extra)}):")
            for key in extra:
                print(f"        + {key}")

    # ── Trailer ────────────────────────────────────────────────────────
    print()
    if total_missing == 0:
        print(f"✓ Every locale is at parity with {SOURCE_LOCALE}.json.")
        if total_extra and not args.no_extras:
            print(
                f"  (Note: {total_extra} extra key(s) across locales — "
                f"they don't fail the check but may indicate drift.)"
            )
        return 0

    print(
        f"✗ {total_missing} missing translation(s) across "
        f"{len(locale_with_missing)} locale(s): "
        f"{', '.join(locale_with_missing)}"
    )
    if total_extra and not args.no_extras:
        print(f"  Plus {total_extra} extra key(s); see per-locale output above.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
