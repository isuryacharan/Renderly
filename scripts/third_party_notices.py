#!/usr/bin/env python3
"""Generate THIRD-PARTY-NOTICES.md: licence notices for everything Renderly distributes.

Renderly's installers bundle Rust crates, npm packages, a bundled ONNX model, a bundled font
and FFmpeg. Permissive licences (MIT, BSD, Apache-2.0, ...) require their copyright notice and
licence text to travel with binary distributions, so this script collects them into one file
at the repository root that the Tauri bundle ships alongside the app.

Usage:
    python scripts/third_party_notices.py                 # regenerate THIRD-PARTY-NOTICES.md
    python scripts/third_party_notices.py --check         # fail if the committed file is out of date
    python scripts/third_party_notices.py --fetch-missing # vendor upstream licence texts, then regenerate

Sources:
    Rust  - `cargo metadata --locked`, walking the resolve graph from the shipped workspace
            members over normal (non-dev, non-build) dependency edges. Target-specific
            dependencies are included for every target (cargo metadata does not filter by host),
            which over-approximates the host build: it is the safe direction for notices.
            Fuzzing-only edges are not shipped: an edge whose `cfg(...)` target condition is
            false when `fuzzing` is off (for example `cfg(fuzzing)`) does not count, so a crate
            reachable only through such edges (libfuzzer-sys) is left out. Conditions on other
            predicates are treated as possibly true and kept.
    npm   - production dependencies of renderly-app from renderly-app/package-lock.json, read
            from renderly-app/node_modules (run `npm ci` in renderly-app first).
    Vendored - packages whose crates.io or npm source has no licence file read their licence
            texts from third_party/licenses/<name>/, written by --fetch-missing from the
            package's upstream repository (see `fetch_missing`). Local source always wins.
    Fixed - entries for bundled assets that are not package-managed (YuNet, Roboto, FFmpeg),
            hard-coded below with the file each licence text comes from. The Roboto copyright
            line is read from the font's own name table at generation time.

Standard library only. Output is sorted and contains no timestamps or absolute paths, so
re-running on unchanged inputs produces an identical file.
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from datetime import date
from pathlib import Path
from typing import Iterable
from urllib.parse import quote

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "THIRD-PARTY-NOTICES.md"
APP_DIR = REPO_ROOT / "renderly-app"
VENDORED_DIR = REPO_ROOT / "third_party" / "licenses"
VENDORED_MARKER = "SOURCE.txt"
GITHUB_API = "https://api.github.com"
USER_AGENT = "renderly-third-party-notices"

# Shipped workspace members whose normal dependency graphs make up the distributed binaries.
# renderly-wasm is included because its wasm32 dependencies (wgpu, wasm-bindgen, web-sys) are
# compiled into the webview bundle under renderly-app/public/wasm-pkg, which is shipped.
SHIPPED_ROOTS = ("renderly-app", "renderly-cli", "renderly-mcp", "renderly-wasm")

# Licence identifiers a distributed package may carry without a human decision.
ALLOWED_SPDX = frozenset({
    "MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "0BSD",
    "Unicode-3.0", "Unicode-DFS-2016", "BSL-1.0", "CC0-1.0", "Unlicense", "MPL-2.0",
    "BlueOak-1.0.0", "CDLA-Permissive-2.0", "WTFPL", "OFL-1.1",
})
# Allowed, but file-level copyleft: listed in the review section for visibility.
VISIBILITY_SPDX = frozenset({"MPL-2.0"})

LICENCE_FILE_PATTERNS = ("LICENSE", "LICENCE", "COPYING", "NOTICE")

# Fixed entries: bundled files that no package manager describes.
# (display name, version/note, licence SPDX id, licence file relative to repo root, source note)
FIXED_ENTRIES = (
    {
        "name": "YuNet face detection model (face_detection_yunet_2026may.onnx)",
        "spdx": "MIT",
        "licence_file": "renderly-core/assets/models/LICENSE-yunet-opencv-zoo",
        "source": (
            "Bundled in renderly-core via `include_bytes!` (`renderly-core/src/ml/models.rs`). "
            "Model origin: https://github.com/ShiqiYu/libfacedetection.train; distributed copy "
            "from opencv/opencv_zoo (see `renderly-core/assets/models/README.md`)."
        ),
    },
    {
        "name": "Roboto Bold font (Roboto-Bold.ttf)",
        "spdx": "Apache-2.0",
        "licence_file": "renderly-wasm/assets/LICENSE-Roboto.txt",
        "font": "renderly-wasm/assets/Roboto-Bold.ttf",
        "source": (
            "Bundled in renderly-core via `include_bytes!` (`renderly-core/src/captions/mod.rs`), "
            "from `renderly-wasm/assets/Roboto-Bold.ttf`."
        ),
    },
)

FFMPEG_NOTICE = (
    "FFmpeg (`ffmpeg` and `ffprobe`) is bundled into the installers as separate programs. "
    "Its licence, source offer and build details are in [NOTICE-ffmpeg.md](NOTICE-ffmpeg.md)."
)

RVM_NOTE = (
    "Robust Video Matting weights (used for background removal) are **not** distributed with "
    "Renderly. They are downloaded from the upstream host on first use, after the user "
    "consents, and pinned by SHA-256 (see `docs/ml-licence-review.md`). The model is licensed "
    "GPL-3.0 by its upstream; no copy of it is included in this file or the installers."
)

HEADER = """# Third-party notices

This file lists third-party software and data that Renderly distributes in its installers, with
the licence each one is provided under. It is **generated** by
`python scripts/third_party_notices.py` from the dependency graph and the package sources; do not
edit it by hand. Rust entries cover every target-specific dependency of the shipped crates, not
only the host target, so the list is a superset of what any single installer contains.

Renderly itself is licensed under the GNU Affero General Public License v3.0 or later (see
[LICENSE](LICENSE)).
"""


# --------------------------------------------------------------------------- SPDX expressions

_SPDX_TOKEN = re.compile(r"\(|\)|/|[A-Za-z0-9.+\-]+")


class SpdxError(ValueError):
    """Raised when a licence string cannot be parsed as an SPDX expression."""


def parse_spdx(expression: str) -> tuple:
    """Parse an SPDX licence expression into a tree of ('id', x), ('and', [...]), ('or', [...]).

    `X WITH Y` parses to the base licence `X`: an exception only narrows the licence, so the base
    id decides whether the package is acceptable. A legacy `/` separator means OR.
    """
    tokens = _SPDX_TOKEN.findall(expression)
    if not tokens:
        raise SpdxError(f"empty licence expression: {expression!r}")
    pos = 0

    def peek() -> str | None:
        return tokens[pos] if pos < len(tokens) else None

    def take() -> str:
        nonlocal pos
        if pos >= len(tokens):
            raise SpdxError(f"unexpected end of licence expression: {expression!r}")
        pos += 1
        return tokens[pos - 1]

    def parse_or():
        terms = [parse_and()]
        while peek() is not None and (peek().upper() == "OR" or peek() == "/"):
            take()
            terms.append(parse_and())
        return terms[0] if len(terms) == 1 else ("or", terms)

    def parse_and():
        terms = [parse_atom()]
        while peek() is not None and peek().upper() == "AND":
            take()
            terms.append(parse_atom())
        return terms[0] if len(terms) == 1 else ("and", terms)

    def parse_atom():
        token = take()
        if token == "(":
            node = parse_or()
            if take() != ")":
                raise SpdxError(f"unbalanced parentheses: {expression!r}")
            return node
        if token in (")", "/") or token.upper() in ("AND", "OR", "WITH"):
            raise SpdxError(f"unexpected token {token!r} in: {expression!r}")
        if peek() is not None and peek().upper() == "WITH":
            take()
            take()  # the exception id; ignored, see docstring
        return ("id", token)

    tree = parse_or()
    if pos != len(tokens):
        raise SpdxError(f"trailing tokens in licence expression: {expression!r}")
    return tree


def spdx_satisfiable(tree: tuple, allowed: frozenset[str]) -> bool:
    """True if the expression can be satisfied using only identifiers in `allowed`."""
    kind = tree[0]
    if kind == "id":
        return tree[1] in allowed
    if kind == "and":
        return all(spdx_satisfiable(child, allowed) for child in tree[1])
    return any(spdx_satisfiable(child, allowed) for child in tree[1])


def spdx_ids(tree: tuple) -> set[str]:
    """Every licence identifier mentioned in the expression."""
    if tree[0] == "id":
        return {tree[1]}
    return set().union(*(spdx_ids(child) for child in tree[1]))


# ----------------------------------------------------------------------------- data records


@dataclass(eq=False)
class Package:
    """One distributed package and its licence material."""

    ecosystem: str  # "Rust" or "npm"
    name: str
    version: str
    spdx: str | None  # the declared licence expression, as written by the package
    repository: str
    licence_files: list[Path] = field(default_factory=list)
    note: str = ""  # shown in the licence-text column when no file was found
    vcs_path: str = ""  # Rust: directory of the crate inside its repository (.cargo_vcs_info.json)
    vcs_sha: str = ""  # Rust: commit the crate was published from (.cargo_vcs_info.json)
    vendored_from: str = ""  # set when the licence texts come from third_party/licenses

    @property
    def key(self) -> tuple[str, str, str]:
        return (self.ecosystem, self.name, self.version)


@dataclass
class Review:
    """A package that needs a human look before the file is considered final."""

    package: Package
    reason: str
    blocking: bool  # False for informational entries such as MPL-2.0 visibility


# ----------------------------------------------------------------------------- file helpers


def normalize_text(raw: bytes) -> str:
    """Decode a licence file and normalise line endings so identical texts hash identically."""
    text = raw.decode("utf-8", errors="replace").replace("\r\n", "\n").replace("\r", "\n")
    return text.strip() + "\n"


def find_licence_files(package_dir: Path, extra: Iterable[str] = ()) -> list[Path]:
    """Top-level LICENSE*/LICENCE*/COPYING*/NOTICE* files (case-insensitive), plus any extra."""
    found: list[Path] = []
    if package_dir.is_dir():
        for entry in sorted(package_dir.iterdir(), key=lambda p: p.name.lower()):
            if not entry.is_file():
                continue
            upper = entry.name.upper()
            if any(upper.startswith(prefix) for prefix in LICENCE_FILE_PATTERNS):
                found.append(entry)
    for name in extra:
        candidate = package_dir / name
        if candidate.is_file() and candidate not in found:
            found.append(candidate)
    return found


def clean_repo_url(url: str | None) -> str:
    """Strip a `git+` prefix and a trailing `.git` so the URL reads cleanly."""
    if not url:
        return ""
    url = url.strip()
    if url.startswith("git+"):
        url = url[len("git+"):]
    if url.endswith(".git"):
        url = url[: -len(".git")]
    return url


# ----------------------------------------------------------------------------- Rust


def run_cargo_metadata() -> dict:
    """Run `cargo metadata --locked` against the workspace and return the parsed JSON."""
    cargo = shutil.which("cargo")
    if cargo is None:
        raise SystemExit("error: cargo not found on PATH")
    result = subprocess.run(
        [cargo, "metadata", "--format-version", "1", "--locked"],
        cwd=REPO_ROOT,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if result.returncode != 0:
        raise SystemExit(f"error: cargo metadata failed:\n{result.stderr}")
    return json.loads(result.stdout)


_CFG_TOKEN = re.compile(r'[(),=]|[A-Za-z_][A-Za-z0-9_]*|"[^"]*"')


def cfg_outcome(condition: str | None) -> bool | None:
    """Evaluate a Cargo target condition for a shipped (non-fuzzing) build.

    `fuzzing` is false; every other predicate is unknown. Returns True or False when the
    condition is decided, None when it depends on something this script does not model (a
    target triple, `target_os = "..."`, and so on). Unknown conditions are kept, which is the
    safe direction for notices.
    """
    if condition is None:
        return True
    condition = condition.strip()
    if not condition.startswith("cfg("):
        return None  # a bare target triple
    tokens = _CFG_TOKEN.findall(condition)
    pos = 0

    def parse() -> bool | None:
        nonlocal pos
        name = tokens[pos]
        pos += 1
        if pos < len(tokens) and tokens[pos] == "=":
            pos += 2  # key = "value": not modelled
            return None
        if pos >= len(tokens) or tokens[pos] != "(":
            return False if name == "fuzzing" else None
        pos += 1  # consume "("
        args: list[bool | None] = []
        while tokens[pos] != ")":
            args.append(parse())
            if tokens[pos] == ",":
                pos += 1
        pos += 1  # consume ")"
        if name == "cfg":
            return args[0]
        if name == "not":
            return None if args[0] is None else not args[0]
        if name == "all":
            if False in args:
                return False
            return True if all(a is True for a in args) else None
        if name == "any":
            if True in args:
                return True
            return False if all(a is False for a in args) else None
        return None

    try:
        return parse()
    except (IndexError, ValueError):
        return None


def has_normal_edge(dep: dict) -> bool:
    """A resolve edge is shipped when one of its `normal` dependency kinds can be true in a
    shipped build. Edges gated on `cfg(fuzzing)` (libFuzzer harnesses) are not shipped."""
    return any(
        kind.get("kind") is None and cfg_outcome(kind.get("target")) is not False
        for kind in dep.get("dep_kinds", [])
    )


def rust_packages(metadata: dict) -> list[Package]:
    """Collect every non-workspace package reachable from SHIPPED_ROOTS over normal edges."""
    workspace_ids = set(metadata["workspace_members"])
    packages_by_id = {pkg["id"]: pkg for pkg in metadata["packages"]}
    nodes_by_id = {node["id"]: node for node in metadata["resolve"]["nodes"]}

    roots = [
        pkg_id for pkg_id in workspace_ids
        if packages_by_id[pkg_id]["name"] in SHIPPED_ROOTS
    ]
    missing = set(SHIPPED_ROOTS) - {packages_by_id[r]["name"] for r in roots}
    if missing:
        raise SystemExit(f"error: shipped workspace members not found: {sorted(missing)}")

    reachable: set[str] = set()
    stack = list(roots)
    while stack:
        current = stack.pop()
        if current in reachable:
            continue
        reachable.add(current)
        for dep in nodes_by_id[current]["deps"]:
            if has_normal_edge(dep) and dep["pkg"] not in reachable:
                stack.append(dep["pkg"])

    packages: list[Package] = []
    for pkg_id in sorted(reachable - workspace_ids):
        pkg = packages_by_id[pkg_id]
        manifest_dir = Path(pkg["manifest_path"]).parent
        extra = [pkg["license_file"]] if pkg.get("license_file") else []
        vcs_path, vcs_sha = read_vcs_info(manifest_dir)
        packages.append(Package(
            ecosystem="Rust",
            name=pkg["name"],
            version=pkg["version"],
            spdx=pkg.get("license"),
            repository=clean_repo_url(pkg.get("repository") or pkg.get("homepage")),
            licence_files=find_licence_files(manifest_dir, extra),
            vcs_path=vcs_path,
            vcs_sha=vcs_sha,
        ))
    return packages


def read_vcs_info(crate_dir: Path) -> tuple[str, str]:
    """(path in repository, commit SHA) that crates.io recorded in `.cargo_vcs_info.json`."""
    info_path = crate_dir / ".cargo_vcs_info.json"
    if not info_path.is_file():
        return "", ""
    info = json.loads(info_path.read_text(encoding="utf-8"))
    return info.get("path_in_vcs", "") or "", (info.get("git") or {}).get("sha1", "") or ""


# ----------------------------------------------------------------------------- npm


def npm_package_dir(lock_packages: dict, from_key: str, name: str) -> str | None:
    """Resolve `name` as seen from the lockfile entry `from_key`, walking up node_modules."""
    base = from_key
    while True:
        candidate = f"{base}/node_modules/{name}" if base else f"node_modules/{name}"
        if candidate in lock_packages:
            return candidate
        if not base:
            return None
        # Strip the last `/node_modules/<name>` segment and try the enclosing scope.
        index = base.rfind("/node_modules/")
        base = base[:index] if index != -1 else ""


def npm_read_licence(install_dir: Path) -> tuple[str | None, str, str]:
    """(SPDX-ish licence, version, repository URL) from package.json, accepting legacy forms."""
    manifest = json.loads((install_dir / "package.json").read_text(encoding="utf-8"))
    licence = manifest.get("license")
    if isinstance(licence, dict):
        licence = licence.get("type")
    if not licence and isinstance(manifest.get("licenses"), list) and manifest["licenses"]:
        first = manifest["licenses"][0]
        licence = first.get("type") if isinstance(first, dict) else str(first)
    version = manifest.get("version", "")
    repo = manifest.get("repository")
    if isinstance(repo, dict):
        repo = repo.get("url")
    return (licence or None), version, clean_repo_url(repo if isinstance(repo, str) else "")


def npm_packages() -> list[Package]:
    """Production dependencies of renderly-app, transitively, from its lockfile."""
    lock = json.loads((APP_DIR / "package-lock.json").read_text(encoding="utf-8"))
    lock_packages: dict = lock["packages"]
    root = lock_packages[""]

    collected: dict[str, Package] = {}
    stack: list[tuple[str, str]] = []  # (lockfile key of the dependent, dependency name)
    for name in root.get("dependencies", {}):
        stack.append(("", name))
    for name in root.get("optionalDependencies", {}):
        stack.append(("", name))

    visited: set[str] = set()
    while stack:
        from_key, name = stack.pop()
        key = npm_package_dir(lock_packages, from_key, name)
        if key is None:
            raise SystemExit(f"error: npm dependency {name!r} (from {from_key or 'root'}) is not in the lockfile")
        if key in visited:
            continue
        visited.add(key)
        entry = lock_packages[key]
        if entry.get("dev"):
            continue
        install_dir = REPO_ROOT / "renderly-app" / key
        if not install_dir.is_dir():
            if entry.get("optional"):
                continue  # platform-specific optional dependency, not installed on this host
            raise SystemExit(f"error: {key} is missing from node_modules; run `npm ci` in renderly-app")
        spdx, version, repo = npm_read_licence(install_dir)
        collected[key] = Package(
            ecosystem="npm",
            name=name,
            version=entry.get("version", version),
            spdx=spdx,
            repository=repo,
            licence_files=find_licence_files(install_dir),
        )
        for child in list(entry.get("dependencies", {})) + list(entry.get("optionalDependencies", {})):
            stack.append((key, child))

    return sorted(collected.values(), key=lambda p: (p.name, p.version))


# ----------------------------------------------------------------------------- review


def review_flags(package: Package) -> list[Review]:
    """Return the review reasons for one package (empty when it is clearly acceptable)."""
    flags: list[Review] = []
    if not package.spdx:
        flags.append(Review(package, "no licence expression declared", blocking=True))
        return flags
    try:
        tree = parse_spdx(package.spdx)
    except SpdxError as error:
        flags.append(Review(package, f"licence expression not parseable: {error}", blocking=True))
        return flags
    if not spdx_satisfiable(tree, ALLOWED_SPDX):
        blocking = sorted(spdx_ids(tree) - ALLOWED_SPDX)
        flags.append(Review(
            package,
            "not satisfiable with the permitted licence list; identifiers outside it: "
            + ", ".join(blocking),
            blocking=True,
        ))
    if spdx_ids(tree) & VISIBILITY_SPDX:
        flags.append(Review(
            package,
            "file-level copyleft (MPL-2.0): compatible, listed for visibility",
            blocking=False,
        ))
    return flags


# ----------------------------------------------------------------------------- rendering


def build_licence_texts(packages: list[Package]) -> tuple[dict[str, int], list[tuple[int, str, list[str]]], dict[Package, list[int]]]:
    """Deduplicate licence texts. Returns (hash -> id, [(id, text, users)], package -> ids)."""
    ids_by_hash: dict[str, int] = {}
    texts: list[tuple[int, str, list[str]]] = []
    package_ids: dict[Package, list[int]] = {}
    for package in packages:
        ids: list[int] = []
        for path in package.licence_files:
            text = normalize_text(path.read_bytes())
            digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            if digest not in ids_by_hash:
                ids_by_hash[digest] = len(ids_by_hash) + 1
                texts.append((ids_by_hash[digest], text, []))
            text_id = ids_by_hash[digest]
            if text_id not in ids:
                ids.append(text_id)
            users = texts[text_id - 1][2]
            label = f"{package.name} {package.version}"
            if label not in users:
                users.append(label)
        package_ids[package] = ids
    return ids_by_hash, texts, package_ids


def cell(value: str) -> str:
    """Make a value safe inside a Markdown table cell."""
    return value.replace("|", "\\|").replace("\n", " ").strip()


def render(rust: list[Package], npm: list[Package], fixed_texts: list[tuple[str, str, str, str]]) -> tuple[str, list[Review]]:
    """Build the full Markdown document. Returns (document, review list)."""
    all_packages = sorted(rust + npm, key=lambda p: (p.ecosystem, p.name.lower(), p.version))
    reviews: list[Review] = []
    for package in all_packages:
        reviews.extend(review_flags(package))

    for package in all_packages:
        if not package.licence_files:
            print(
                f"WARNING: {package.ecosystem} {package.name} {package.version} has no licence file "
                f"in its source (declared: {package.spdx or 'none'}).",
                file=sys.stderr,
            )
    for review in reviews:
        level = "WARNING: NEEDS REVIEW" if review.blocking else "NOTE"
        print(
            f"{level}: {review.package.ecosystem} {review.package.name} "
            f"{review.package.version} ({review.package.spdx or 'no licence'}): {review.reason}",
            file=sys.stderr,
        )

    _, texts, package_ids = build_licence_texts(all_packages)
    fixed_text_ids: dict[str, int] = {}  # name -> licence text id
    for name, spdx, licence_text, _source in fixed_texts:
        digest = hashlib.sha256(licence_text.encode("utf-8")).hexdigest()
        existing = [tid for tid, text, _ in texts if hashlib.sha256(text.encode("utf-8")).hexdigest() == digest]
        if existing:
            fixed_text_ids[name] = existing[0]
        else:
            new_id = len(texts) + 1
            texts.append((new_id, licence_text, [name]))
            fixed_text_ids[name] = new_id

    out: list[str] = [HEADER]

    out.append("## Needs review\n")
    if reviews:
        out.append(
            "The following entries have a licence that is not on Renderly's permitted list, or need "
            "a note for visibility. A human decision is required before these are considered "
            "cleared for distribution.\n"
        )
        out.append("| Package | Version | Licence | Reason |")
        out.append("|---|---|---|---|")
        for review in reviews:
            p = review.package
            out.append(f"| {cell(p.name)} | {cell(p.version)} | {cell(p.spdx or '(none)')} | {cell(review.reason)} |")
        out.append("")
    else:
        out.append("None. Every packaged dependency declares a licence on the permitted list.\n")

    rust_count = len(rust)
    npm_count = len(npm)
    out.append("## Summary\n")
    out.append(f"- Rust packages: {rust_count}")
    out.append(f"- npm packages: {npm_count}")
    out.append(f"- Fixed entries (bundled model and font): {len(FIXED_ENTRIES)}")
    out.append(f"- Distinct licence texts: {len(texts)}")
    out.append("")

    def package_table(title: str, packages: list[Package]) -> None:
        out.append(f"## {title}\n")
        out.append("| Package | Version | Licence (SPDX) | Repository | Licence text |")
        out.append("|---|---|---|---|---|")
        for package in packages:
            ids = package_ids[package]
            if ids:
                text_ref = ", ".join(f"[T{i}](#licence-text-{i})" for i in ids)
                if package.vendored_from:
                    text_ref += f" (vendored from upstream, {package.vendored_from})"
            elif package.spdx:
                text_ref = f"No licence file in source; licence text: see {package.spdx} standard text"
            else:
                text_ref = "No licence file in source and no licence declared"
            out.append(
                f"| {cell(package.name)} | {cell(package.version)} | {cell(package.spdx or '(none)')} "
                f"| {cell(package.repository)} | {text_ref} |"
            )
        out.append("")

    package_table("Rust crates", rust)
    package_table("npm packages", npm)

    out.append("## Bundled model, font and FFmpeg\n")
    for name, spdx, _text, source in fixed_texts:
        out.append(f"### {name}\n")
        out.append(f"- Licence: {spdx}")
        out.append(f"- {source}")
        out.append(f"- Licence text: [T{fixed_text_ids[name]}](#licence-text-{fixed_text_ids[name]})")
        out.append("")
    out.append("### FFmpeg\n")
    out.append(FFMPEG_NOTICE + "\n")
    out.append("### Robust Video Matting (downloaded, not distributed)\n")
    out.append(RVM_NOTE + "\n")
    out.append("Test-only fixtures (for example OpenTimelineIO sample files) are not distributed and are "
               "not listed here.\n")

    out.append("## Licence texts\n")
    out.append("Each distinct licence text appears once. Packages reference it by its number.\n")
    for text_id, text, users in texts:
        out.append(f"<a id=\"licence-text-{text_id}\"></a>\n")
        out.append(f"### T{text_id}\n")
        out.append(f"Used by: {', '.join(sorted(users, key=str.lower))}\n")
        out.append("````text")
        out.append(text.rstrip("\n"))
        out.append("````\n")

    return "\n".join(out).rstrip("\n") + "\n", reviews


def read_font_names(path: Path) -> dict[int, str]:
    """Name-table strings of an OpenType/TrueType font, keyed by name ID.

    Prefers the Windows en-US record, then other Windows records, then anything else.
    """
    data = path.read_bytes()
    if data[:4] not in (b"\x00\x01\x00\x00", b"true", b"OTTO"):
        raise SystemExit(f"error: {path} is not an OpenType/TrueType font")
    (num_tables,) = struct.unpack_from(">H", data, 4)
    name_table = None
    for index in range(num_tables):
        tag, _checksum, offset, _length = struct.unpack_from(">4sIII", data, 12 + 16 * index)
        if tag == b"name":
            name_table = offset
            break
    if name_table is None:
        raise SystemExit(f"error: {path} has no name table")
    _format, count, string_offset = struct.unpack_from(">HHH", data, name_table)
    best: dict[int, tuple[int, str]] = {}
    for index in range(count):
        platform, _encoding, language, name_id, length, offset = struct.unpack_from(
            ">HHHHHH", data, name_table + 6 + 12 * index
        )
        start = name_table + string_offset + offset
        raw = data[start:start + length]
        if platform in (0, 3):
            text = raw.decode("utf-16-be", errors="replace")
        else:
            text = raw.decode("latin-1", errors="replace")
        rank = 0 if (platform == 3 and language == 0x409) else (1 if platform == 3 else 2)
        if name_id not in best or rank < best[name_id][0]:
            best[name_id] = (rank, text)
    return {name_id: text for name_id, (_rank, text) in best.items()}


def fixed_entries() -> list[tuple[str, str, str, str]]:
    """Read the licence text for every fixed entry; fail loudly if a file is missing.

    An entry with a `font` reads its copyright notice (name ID 0) and licence description
    (name ID 13) from that font. The copyright line must appear in the licence text, so the
    licence file cannot drift from the font it covers.
    """
    entries = []
    for entry in FIXED_ENTRIES:
        path = REPO_ROOT / entry["licence_file"]
        if not path.is_file():
            raise SystemExit(f"error: licence file for {entry['name']} not found at {entry['licence_file']}")
        text = normalize_text(path.read_bytes())
        source = entry["source"]
        if "font" in entry:
            font_path = REPO_ROOT / entry["font"]
            if not font_path.is_file():
                raise SystemExit(f"error: font for {entry['name']} not found at {entry['font']}")
            names = read_font_names(font_path)
            copyright_line = names.get(0, "").strip()
            description = names.get(13, "").strip()
            if not copyright_line:
                raise SystemExit(f"error: {entry['font']} has no copyright (name ID 0)")
            if copyright_line not in text:
                raise SystemExit(
                    f"error: {entry['licence_file']} does not contain the font's copyright line "
                    f"{copyright_line!r}; add it at the top of the file"
                )
            source += (
                f" Copyright notice, read from the font's name table (ID 0): {copyright_line} "
                f"Licence description (ID 13): {description}."
            )
        entries.append((entry["name"], entry["spdx"], text, source))
    return entries


# ----------------------------------------------------------------------------- vendored licences


def vendored_is_current(package: Package) -> bool:
    """True when third_party/licenses/<name>/ holds a copy recorded for this exact version."""
    marker = VENDORED_DIR / package.name / VENDORED_MARKER
    return marker.is_file() and parse_marker(marker).get("Version") == package.version


def apply_vendored(packages: list[Package]) -> None:
    """Attach vendored licence texts to packages whose own source has no licence file."""
    for package in packages:
        if package.licence_files:
            continue  # local source wins
        directory = VENDORED_DIR / package.name
        if not vendored_is_current(package):
            if (directory / VENDORED_MARKER).is_file():
                print(
                    f"WARNING: {directory.relative_to(REPO_ROOT).as_posix()} does not match "
                    f"{package.name} {package.version}; re-run --fetch-missing.",
                    file=sys.stderr,
                )
            continue
        package.licence_files = find_licence_files(directory)
        package.vendored_from = parse_marker(directory / VENDORED_MARKER).get("Ref", "unknown ref")


def parse_marker(path: Path) -> dict[str, str]:
    """Read the `Key: value` lines of a SOURCE.txt marker."""
    fields: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        key, sep, value = line.partition(":")
        if sep and not line.startswith(" "):
            fields[key.strip()] = value.strip()
    return fields


class GitHubError(Exception):
    """A GitHub request failed for a reason other than the resource being absent."""


class RateLimited(GitHubError):
    """GitHub refused further requests for this hour."""


class GitHub:
    """Minimal GitHub client over urllib. `get` returns None for 404."""

    def __init__(self, token: str | None) -> None:
        self.token = token

    def get(self, url: str, accept: str = "application/vnd.github+json") -> bytes | None:
        headers = {"User-Agent": USER_AGENT, "Accept": accept, "X-GitHub-Api-Version": "2022-11-28"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(url, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            body = error.read().decode("utf-8", errors="replace")
            if error.code == 404:
                return None
            limited = error.code == 429 or (
                error.code == 403 and (error.headers.get("X-RateLimit-Remaining") == "0"
                                       or "rate limit" in body.lower())
            )
            if limited:
                raise RateLimited(f"GitHub rate limit reached (HTTP {error.code})") from error
            raise GitHubError(f"HTTP {error.code} for {url}") from error
        except urllib.error.URLError as error:
            raise GitHubError(f"network error for {url}: {error.reason}") from error

    def json(self, url: str) -> object | None:
        raw = self.get(url)
        return None if raw is None else json.loads(raw.decode("utf-8"))


def github_token() -> str | None:
    """GITHUB_TOKEN, else the token from an authenticated `gh` CLI, else None (60 requests/hour)."""
    token = os.environ.get("GITHUB_TOKEN", "").strip()
    if token:
        return token
    gh = shutil.which("gh")
    if gh is None:
        return None
    result = subprocess.run([gh, "auth", "token"], capture_output=True, encoding="utf-8",
                            errors="replace", check=False)
    token = result.stdout.strip() if result.returncode == 0 else ""
    return token or None


GITHUB_REPO = re.compile(
    r"^https?://github\.com/(?P<owner>[^/]+)/(?P<repo>[^/#?]+?)/?"
    r"(?:(?:tree|blob)/[^/]+/(?P<subpath>.+?))?/?$"
)


def parse_github_url(url: str) -> tuple[str, str, str] | None:
    """(owner, repo, subdirectory) for a github.com repository URL, or None."""
    match = GITHUB_REPO.match(url.rstrip("/"))
    if not match:
        return None
    repo = match.group("repo")
    if repo.endswith(".git"):
        repo = repo[:-len(".git")]
    return match.group("owner"), repo, (match.group("subpath") or "").strip("/")


LICENCE_NAME = re.compile(r"^(?:" + "|".join(LICENCE_FILE_PATTERNS) + r")", re.IGNORECASE)


def candidate_tags(name: str, version: str) -> list[str]:
    """Tag names that usually mark a release, in the order they are tried."""
    tags = [f"v{version}", version, f"{name}-v{version}", f"{name}-{version}"]
    return list(dict.fromkeys(tags))


@dataclass
class FetchResult:
    """Outcome of fetching one package: either its vendored files or the reason it failed."""

    package: Package
    ref: str = ""  # "tag v1.2.3", "commit abcdef012345", or "default branch main"
    path: str = ""  # directory inside the repository the files came from
    files: list[str] = field(default_factory=list)
    urls: dict[str, str] = field(default_factory=dict)  # file name -> raw download URL
    reason: str = ""  # set when unresolved


class Fetcher:
    """Resolves upstream refs and licence files, caching GitHub lookups per repository."""

    def __init__(self, github: GitHub) -> None:
        self.github = github
        self.tag_cache: dict[tuple[str, str, str], bool] = {}
        self.listing_cache: dict[tuple[str, str, str, str], list[dict]] = {}
        self.default_branch_cache: dict[tuple[str, str], str] = {}

    def tag_exists(self, owner: str, repo: str, tag: str) -> bool:
        key = (owner, repo, tag)
        if key not in self.tag_cache:
            url = f"{GITHUB_API}/repos/{owner}/{repo}/git/ref/tags/{quote(tag, safe='')}"
            found = self.github.json(url)
            self.tag_cache[key] = isinstance(found, dict) and found.get("ref") == f"refs/tags/{tag}"
        return self.tag_cache[key]

    def default_branch(self, owner: str, repo: str) -> str:
        key = (owner, repo)
        if key not in self.default_branch_cache:
            info = self.github.json(f"{GITHUB_API}/repos/{owner}/{repo}")
            if not isinstance(info, dict) or not info.get("default_branch"):
                raise GitHubError(f"cannot read default branch of {owner}/{repo}")
            self.default_branch_cache[key] = info["default_branch"]
        return self.default_branch_cache[key]

    def listing(self, owner: str, repo: str, ref: str, path: str) -> list[dict]:
        key = (owner, repo, ref, path)
        if key not in self.listing_cache:
            location = quote(path, safe="/") if path else ""
            url = f"{GITHUB_API}/repos/{owner}/{repo}/contents/{location}?ref={quote(ref, safe='')}"
            entries = self.github.json(url)
            self.listing_cache[key] = entries if isinstance(entries, list) else []
        return self.listing_cache[key]

    def licence_entries(self, owner: str, repo: str, ref: str, path: str) -> list[dict]:
        return [
            entry for entry in self.listing(owner, repo, ref, path)
            if entry.get("type") == "file" and LICENCE_NAME.match(entry.get("name", ""))
        ]

    def fetch(self, package: Package) -> FetchResult:
        result = FetchResult(package)
        if not package.repository:
            result.reason = "no repository URL in the package metadata"
            return result
        parsed = parse_github_url(package.repository)
        if parsed is None:
            result.reason = f"repository is not on github.com ({package.repository}); not fetched"
            return result
        owner, repo, url_subpath = parsed
        subpath = package.vcs_path or url_subpath

        ref = ""
        for tag in candidate_tags(package.name, package.version):
            if self.tag_exists(owner, repo, tag):
                ref, result.ref = tag, f"tag {tag}"
                break
        if not ref and package.vcs_sha:
            ref, result.ref = package.vcs_sha, f"commit {package.vcs_sha[:12]} (crates.io VCS record)"
        if not ref:
            ref = self.default_branch(owner, repo)
            result.ref = f"default branch {ref} (no matching tag or commit)"

        entries = self.licence_entries(owner, repo, ref, subpath) if subpath else []
        result.path = subpath
        if not entries:
            entries = self.licence_entries(owner, repo, ref, "")
            result.path = "(repository root)"
        if not entries:
            where = f"{subpath} or the repository root" if subpath else "the repository root"
            result.reason = f"no LICENSE/LICENCE/COPYING/NOTICE file at {result.ref} in {where}"
            return result
        result.files = sorted(entry["name"] for entry in entries)
        result.urls = {entry["name"]: entry["download_url"] for entry in entries}
        return result


def write_vendored(package: Package, result: FetchResult, fetcher: Fetcher) -> None:
    """Download the licence files of one package into third_party/licenses/<name>/.

    The directory is replaced, so a copy left over from an older version does not survive.
    """
    directory = VENDORED_DIR / package.name
    downloads = {}
    for filename in result.files:
        data = fetcher.github.get(result.urls[filename], accept="application/octet-stream")
        if data is None:
            raise GitHubError(f"download failed for {result.urls[filename]}")
        downloads[filename] = data
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for filename, data in downloads.items():
        (directory / filename).write_bytes(data)
    lines = [
        f"Crate: {package.name}",
        f"Version: {package.version}",
        f"Repository: {package.repository}",
        f"Ref: {result.ref}",
        f"Path: {result.path}",
        "Files:",
        *[f"  - {filename} from {result.urls[filename]}" for filename in result.files],
        f"Fetched: {date.today().isoformat()}",
        "",
    ]
    (directory / VENDORED_MARKER).write_text("\n".join(lines), encoding="utf-8", newline="\n")


def fetch_missing(packages: list[Package]) -> None:
    """Vendor licence texts for packages that have none, from their GitHub repositories.

    Idempotent: a package whose vendored copy already matches its version is skipped. A
    failure is reported with its reason and leaves the package on the warning list.
    """
    missing = [p for p in packages if not p.licence_files and not vendored_is_current(p)]
    if not missing:
        print("nothing to fetch: every package has a licence file or a vendored copy")
        return
    token = github_token()
    print(f"fetching licence texts for {len(missing)} packages "
          f"({'authenticated' if token else 'unauthenticated, 60 requests/hour'})")
    fetcher = Fetcher(GitHub(token))
    unresolved: list[FetchResult] = []
    for package in missing:
        try:
            result = fetcher.fetch(package)
            if result.reason:
                unresolved.append(result)
            else:
                write_vendored(package, result, fetcher)
                print(f"fetched {package.name} {package.version}: {result.ref}, {result.path}")
        except RateLimited as error:
            unresolved.append(FetchResult(package, reason=f"{error}; set GITHUB_TOKEN and re-run"))
            for rest in missing[missing.index(package) + 1:]:
                unresolved.append(FetchResult(rest, reason="not attempted: rate limit reached"))
            break
        except GitHubError as error:
            unresolved.append(FetchResult(package, reason=str(error)))
    print(f"\n{len(missing) - len(unresolved)} fetched, {len(unresolved)} unresolved:")
    for result in unresolved:
        package = result.package
        print(f"  UNRESOLVED {package.ecosystem} {package.name} {package.version}: {result.reason}")


def collect() -> tuple[list[Package], list[Package]]:
    """Rust and npm packages to distribute, with vendored licence texts applied."""
    rust = rust_packages(run_cargo_metadata())
    npm = npm_packages()
    apply_vendored(rust + npm)
    return rust, npm


def generate() -> str:
    """Produce the full notices document as a string."""
    rust, npm = collect()
    document, _ = render(rust, npm, fixed_entries())
    return document


def check(output: Path) -> int:
    """Regenerate into a temporary file and fail if the committed file differs."""
    fresh = generate()
    with tempfile.TemporaryDirectory() as tmp:
        temp_path = Path(tmp) / "THIRD-PARTY-NOTICES.md"
        temp_path.write_text(fresh, encoding="utf-8", newline="\n")
        if not output.is_file():
            print(f"error: {output.name} is missing; run `python scripts/third_party_notices.py`", file=sys.stderr)
            return 1
        committed = output.read_text(encoding="utf-8").replace("\r\n", "\n")
        if committed == fresh:
            print(f"{output.name} is up to date.")
            return 0
        diff = difflib.unified_diff(
            committed.splitlines(keepends=True),
            fresh.splitlines(keepends=True),
            fromfile=f"committed/{output.name}",
            tofile=f"regenerated/{output.name}",
        )
        sys.stdout.writelines(diff)
        print(
            f"\nerror: {output.name} is out of date. Run `python scripts/third_party_notices.py` "
            "and commit the result.",
            file=sys.stderr,
        )
        return 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument(
        "--check",
        action="store_true",
        help="regenerate to a temporary file and fail if the committed THIRD-PARTY-NOTICES.md differs",
    )
    parser.add_argument(
        "--fetch-missing",
        action="store_true",
        help="fetch licence texts for packages with no licence file from their GitHub repositories "
             "into third_party/licenses/, then regenerate THIRD-PARTY-NOTICES.md",
    )
    args = parser.parse_args(argv)
    if args.check:
        return check(OUTPUT_PATH)
    if args.fetch_missing:
        rust, npm = collect()
        fetch_missing(rust + npm)
    OUTPUT_PATH.write_text(generate(), encoding="utf-8", newline="\n")
    print(f"wrote {OUTPUT_PATH.relative_to(REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
