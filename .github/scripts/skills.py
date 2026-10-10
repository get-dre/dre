#!/usr/bin/env python3
"""Build and check the agent skills in skills/.

    skills.py sync                                  inline the shared snippets, copy the practices
    skills.py generate                              write the plugin references from describe.json
    skills.py check                                 every CI check (changes nothing)
    skills.py release-check <tag>                   a `skills-v<version>` tag matches the skills
    skills.py release-notes <tag>                   the release's notes
    skills.py serves-latest <tag>                   exit 0 if the release moves `skills-latest`

The skills are written once and copied where agents need them, because an installed skill is a
folder on its own (installers copy or link each one separately):

- `skills/shared/*.md` are inlined into each SKILL.md between `<!-- BEGIN shared/<file> -->` and
  `<!-- END shared/<file> -->`.
- `docs/practices.md` is copied to `<skill>/references/practices.md`.
- Each first-party plugin's reference, `<skill>/references/plugins/<kind>-<name>.md`, is generated
  from the plugin's `describe` reply, its sections of docs/plugins.md, and the hand-written
  `skills/shared/guide-notes/<kind>-<name>.md`.

`check` fails when any copy is stale, a plugin field or option is missing from the plugin docs or
has no description, a skill names a `dre` command or flag the built `dre` doesn't have, cites a
practice that doesn't exist, names a plugin that doesn't exist, or has invalid frontmatter or
versions. README.md and docs/ are checked for `dre` commands and flags as well.
"""

import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

from docs_sections import strip_nav

ROOT = pathlib.Path(__file__).resolve().parents[2]
PACKAGES = json.loads((ROOT / ".github/scripts/packages.json").read_text())
PLUGIN_DOCS = "docs/plugins.md"
PRACTICES = "docs/practices.md"
DOCS_URL = "https://github.com/get-dre/dre/blob/master/docs/"

# What each skill gets copied into its references/ folder. Every skill folder must be listed.
COPIES = {
    "dre": ["practices", "plugins"],
    "dre-install": [],
    "dre-setup": ["practices", "plugins"],
    "dre-report": ["practices", "plugins"],
    "dre-run": ["practices", "plugins"],
    "dre-upgrade": [],
}

# What each plugin's reference includes from the docs: its own page (`("page", name)`: the
# prose of docs/plugin-<name>.md after the generated block), and sections of docs/plugins.md the
# plugins share, as (## heading, ### heading), None taking the text between the ## heading and its
# first ###. Every field and option a plugin declares must be named (in backticks) in its sections.
_DEST = [("Destinations", None), ("Destinations", "Tries again"), ("Destinations", "Several destinations")]
_EXISTS = [("Destinations", "A file already at the path")]
_FILES = _EXISTS + [("Destinations", "Uploads under a temporary name")]
_FORMAT = [("Formats", None)]
_WAREHOUSE = [("Sources", "Types from warehouses")]
DOC_SECTIONS = {
    "source/duckdb": [("page", "duckdb")],
    "source/postgres": [("page", "postgres")],
    "source/databricks": [("page", "databricks")] + _WAREHOUSE,
    "source/bigquery": [("page", "bigquery")] + _WAREHOUSE,
    "source/snowflake": [("page", "snowflake")] + _WAREHOUSE,
    "format/csv": [("page", "csv")] + _FORMAT,
    "format/delimited": [("page", "csv")] + _FORMAT,
    "format/fixed_width": [("page", "fixed_width")] + _FORMAT,
    "format/parquet": [("page", "parquet")] + _FORMAT,
    "format/xlsx": [("page", "xlsx")] + _FORMAT,
    "destination/s3": [("page", "s3")] + _DEST + _EXISTS,
    "destination/gcs": [("page", "gcs")] + _DEST + _EXISTS,
    "destination/azure_blob": [("page", "azure_blob")] + _DEST + _EXISTS,
    "destination/sftp": [("page", "sftp")] + _DEST + _FILES,
    "destination/ftp": [("page", "ftp")] + _DEST + _FILES,
    "destination/databricks": [("page", "databricks")] + _DEST + _EXISTS,
    "destination/email": [("page", "email")] + _DEST,
    "destination/slack": [("page", "slack")] + _DEST,
    "destination/teams": [("page", "teams")] + _DEST,
    "destination/google_chat": [("page", "google_chat")] + _DEST,
}
GENERATED_END = "<!-- END generated -->"


class Docs:
    """docs/plugins.md and the plugin pages, read once."""

    def __init__(self, root):
        self.root = root
        self.overview = strip_nav((root / PLUGIN_DOCS).read_text())
        self.pages = {}

    def page(self, name):
        if name not in self.pages:
            text = strip_nav((self.root / "docs" / f"plugin-{name}.md").read_text())
            title = re.search(r"^# (.+)$", text, re.M).group(1)
            i = text.find(GENERATED_END)
            self.pages[name] = (title, text[i + len(GENERATED_END):] if i >= 0 else text)
        return self.pages[name]

    def section(self, entry):
        """(title, body, the page links in it are relative to) for one DOC_SECTIONS entry."""
        if entry[0] == "page":
            title, body = self.page(entry[1])
            return title, body, f"plugin-{entry[1]}.md"
        h2, h3 = entry
        return (h3.strip("`") if h3 else h2), doc_section(self.overview, h2, h3), "plugins.md"


# Practice IDs: SEC (secrets), SET (setup), REP (reports), RUN (running and delivery).
PRACTICE_PREFIXES = ("SEC", "SET", "REP", "RUN")
CITED = re.compile(r"\b(?:%s)-\d+\b" % "|".join(PRACTICE_PREFIXES))
DEFINED = re.compile(r"^#{2,4} ((?:%s)-\d+)\b" % "|".join(PRACTICE_PREFIXES), re.M)

SEMVER = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?")
NAME = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


# ---------------------------------------------------------------------------------------------
# Versions


def parse_version(text):
    """A version as a tuple that sorts the way semver does (a pre-release before its release)."""
    m = SEMVER.fullmatch(text.strip())
    if not m:
        raise ValueError(f"`{text}` isn't a version")
    major, minor, patch, pre = m.groups()
    ids = ()
    if pre:
        ids = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split("."))
    return (int(major), int(minor), int(patch), 0 if pre else 1, ids)


def parse_range(text):
    """`>=A, <B` as (lower, upper) version tuples."""
    m = re.fullmatch(r"\s*>=\s*(\S+?)\s*,\s*<\s*(\S+?)\s*", text)
    if not m:
        raise ValueError(f"`{text}` isn't a range like `>=0.1.0, <0.2.0`")
    lower, upper = parse_version(m.group(1)), parse_version(m.group(2))
    if lower >= upper:
        raise ValueError(f"`{text}` is empty")
    return lower, upper


def in_range(version, rng):
    """Whether `version` is in the range. A pre-release of the upper bound (0.2.0-rc.1 for `<0.2.0`)
    is outside it: it already belongs to the next release."""
    v, (lower, upper) = parse_version(version), rng
    below = v[:3] < upper[:3] if upper[3] else v < upper
    return lower <= v and below


def covers(version):
    """The plugin versions a reference generated from `version` covers: up to the next major."""
    return f">={version}, <{parse_version(version)[0] + 1}.0.0"


def workspace_version():
    text = (ROOT / "Cargo.toml").read_text()
    return re.search(r'\[workspace\.package\][^\[]*?\nversion = "([^"]+)"', text).group(1)


def package_version(package):
    about = PACKAGES[package]
    if about.get("go"):
        return (ROOT / about["go"] / "VERSION").read_text().strip()
    text = (ROOT / "plugins" / package / "Cargo.toml").read_text()
    return re.search(r'^version = "([^"]+)"', text, re.M).group(1)


# ---------------------------------------------------------------------------------------------
# Frontmatter


def frontmatter(text):
    """The `---` block of a SKILL.md (flat keys, plus one level of nesting) and the body."""
    m = re.match(r"---\n(.*?)\n---\n", text, re.S)
    if not m:
        raise ValueError("no frontmatter (a `---` block at the top)")
    fm, current = {}, None
    for line in m.group(1).splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        kv = re.fullmatch(r"(\s*)([A-Za-z0-9_-]+):\s*(.*)", line)
        if not kv:
            raise ValueError(f"can't read frontmatter line `{line}`")
        indent, key, value = kv.groups()
        value = _scalar(value)
        if indent:
            if current is None:
                raise ValueError(f"`{key}` is indented under nothing")
            fm[current][key] = value
        elif value == "":
            fm[key], current = {}, key
        else:
            fm[key], current = value, None
    return fm, text[m.end():]


def _scalar(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    # What YAML reads differently from plain text; quote such a value.
    if ": " in value or " #" in value or (value and value[0] in "&*!|>%@`[]{},?-\"'#"):
        raise ValueError(f"quote this value, which isn't plain YAML text: `{value}`")
    return value


def frontmatter_problems(folder, fm):
    problems = []
    name = fm.get("name")
    if not name:
        problems.append("`name` is missing")
    elif name != folder or not NAME.match(name) or len(name) > 64:
        problems.append(f"`name` must be the folder's name, `{folder}` (lowercase letters, digits and hyphens)")
    desc = fm.get("description")
    if not isinstance(desc, str) or not desc.strip():
        problems.append("`description` is missing")
    elif len(desc) > 1024:
        problems.append(f"`description` is {len(desc)} characters; the limit is 1024")
    if "metadata" in fm and not isinstance(fm["metadata"], dict):
        problems.append("`metadata` must be a map")
    return problems


# ---------------------------------------------------------------------------------------------
# `dre` commands a skill mentions


def parse_help(text):
    """The subcommands and flags a clap `--help` lists."""
    subs, flags, section = set(), set(), None
    for line in text.splitlines():
        if re.match(r"^[A-Z][A-Za-z ]*:$", line):
            section = line[:-1]
            continue
        if section == "Commands":
            m = re.match(r"^  ([a-z][a-z0-9-]*)(\s{2,}|$)", line)
            if m:
                subs.add(m.group(1))
        elif section == "Options":
            m = re.match(r"^\s{2,}(-[A-Za-z])?(?:, )?(--[a-z][a-z0-9-]*)?(?=[\s=]|$)", line)
            if m:
                flags.update(f for f in m.groups() if f)
    return subs, flags


def _valued_flags(text):
    """Flags that take a required value (`--select <SELECTOR>`), whose next token isn't a command."""
    return set(re.findall(r"(--[a-z][a-z0-9-]*) <", text)) | set(re.findall(r"^\s+(-[A-Za-z]), --[a-z][a-z0-9-]* <", text, re.M))


def _code(md):
    """The text of every fenced block and inline code span."""
    out = []
    fenced = re.compile(r"^(```|~~~)[^\n]*\n(.*?)^\1[ \t]*$", re.M | re.S)
    for m in fenced.finditer(md):
        out.append(m.group(2))
    rest = fenced.sub("", md)
    out.extend(re.findall(r"`([^`\n]+)`", rest))
    return out


def dre_mentions(md):
    """Each `dre ...` invocation in a Markdown file's code, as its argument tokens."""
    found = []
    for code in _code(md):
        for line in code.splitlines():
            for m in re.finditer(r"(?<![\w./~$-])dre(?=[ \t])", line):
                rest = re.split(r"\s#|&&|\|\||[|;)`]", line[m.end():])[0]
                tokens = rest.split()
                # `dre 0.1.0` is what `dre --version` prints, not a command.
                if tokens and not tokens[0][0].isdigit():
                    found.append(tokens)
    return found


class Cli:
    """`dre`'s commands and flags, read from `--help` through `help_for(path)`."""

    def __init__(self, help_for):
        self.help_for = help_for
        self.nodes = {}

    def node(self, path):
        key = tuple(path)
        if key not in self.nodes:
            text = self.help_for(list(path))
            subs, flags = parse_help(text)
            self.nodes[key] = (subs, flags, _valued_flags(text))
        return self.nodes[key]

    def check(self, tokens):
        """None if `dre <tokens>` names real commands and flags, else what's wrong."""
        path, i = [], 0
        while i < len(tokens):
            t = tokens[i]
            if t.startswith("<") or "{" in t or "$" in t:
                return None  # a placeholder: what follows can't be checked
            subs, flags, valued = self.node(path)
            if t.startswith("-") and t != "-":
                name = t.split("=", 1)[0]
                if name not in flags:
                    return f"`{' '.join(['dre'] + path)}` has no `{name}` option"
                i += 2 if name in valued and "=" not in t else 1
                continue
            if subs:
                if t not in subs:
                    return f"`dre {' '.join(path + [t])}` isn't a dre command"
                path.append(t)
            i += 1
        return None


def unknown_plugins(md, known):
    """`type: x` and `format: x` in a file's code that name no known plugin."""
    return [f"`{m.group(0)}`" for code in _code(md)
            for m in re.finditer(r"\b(?:type|format):\s*([a-z_]+)\b", code) if m.group(1) not in known]


def command_problems(cli, rel, md):
    """Each `dre` command or flag in a Markdown file that the built `dre` doesn't have."""
    return [f"{rel}: {err} (in `dre {' '.join(tokens)}`)"
            for tokens in dre_mentions(md) for err in [cli.check(tokens)] if err]


class ReferenceCli(Cli):
    """`dre`'s commands and flags, read from docs/cli-reference.md (generated from the CLI
    definitions and checked by the `cli_reference` test), so no `dre` needs building."""

    def __init__(self, md):
        self.nodes = {}
        commands, current, global_flags = {}, None, (set(), set())
        for line in md.splitlines():
            m = re.match(r"^## `dre(?: ([a-z ][a-z0-9 -]*))?`$", line)
            if m:
                current = tuple((m.group(1) or "").split())
                commands[current] = (set(), set())
                continue
            if line.startswith("## Global options"):
                current = "global"
                continue
            if line.startswith("#"):
                current = None
                continue
            row = re.match(r"^\| `([^`]+)` \|", line)
            if current is None or not row:
                continue
            flags, valued = global_flags if current == "global" else commands[current]
            for part in row.group(1).split(", "):
                name = part.split(" ")[0]
                if name.startswith("-"):
                    flags.add(name)
                    if "<" in part:
                        valued.add(name)
        for path, (flags, valued) in commands.items():
            subs = {c[len(path)] for c in commands if len(c) == len(path) + 1 and c[:len(path)] == path}
            self.nodes[path] = (subs, flags | global_flags[0] | {"-h", "--help"}, valued | global_flags[1])
        root = self.nodes.setdefault((), (set(), set(), set()))
        root[0].update(c[0] for c in commands if len(c) == 1)
        root[1].update(global_flags[0] | {"-h", "--help", "-V", "--version"})
        root[2].update(global_flags[1])

    def node(self, path):
        return self.nodes.get(tuple(path), (set(), {"-h", "--help"}, set()))


# ---------------------------------------------------------------------------------------------
# Practices


def defined_practices(md):
    return set(DEFINED.findall(md))


def cited_practices(md):
    return set(CITED.findall(md))


# ---------------------------------------------------------------------------------------------
# Plugin docs and references


def doc_section(md, h2, h3):
    """The body of `## h2` (up to its first ###) or of `### h3` inside it, without the heading."""
    lines, fence = md.splitlines(keepends=True), False
    headings = []  # (level, title, index)
    for i, line in enumerate(lines):
        if line.startswith(("```", "~~~")):
            fence = not fence
        elif not fence:
            m = re.match(r"^(#{1,6}) (.*?)\s*$", line)
            if m:
                headings.append((len(m.group(1)), m.group(2), i))
    try:
        start = next(k for k, h in enumerate(headings) if h[0] == 2 and h[1] == h2)
    except StopIteration:
        raise KeyError(f"no `## {h2}` in the plugin docs")
    if h3 is not None:
        found = None
        for k in range(start + 1, len(headings)):
            if headings[k][0] <= 2:
                break
            if headings[k][0] == 3 and headings[k][1] == h3:
                found = k
                break
        if found is None:
            raise KeyError(f"no `### {h3}` under `## {h2}` in the plugin docs")
        start = found
    level = headings[start][0]
    end = len(lines)
    for lvl, _, idx in headings[start + 1:]:
        if lvl <= level or (h3 is None and lvl == 3):
            end = idx
            break
    return "".join(lines[headings[start][2] + 1:end])


def undocumented(plugin, fields, docs_text):
    problems = []
    for f in fields:
        if not (f.get("description") or "").strip():
            problems.append(f"`{plugin}`: `{f['name']}` has no description in its `describe` reply")
        elif f"`{f['name']}`" not in docs_text:
            problems.append(f"`{plugin}`: `{f['name']}` isn't in its docs ({PLUGIN_DOCS} or its plugin page)")
    return problems


def reference_name(plugin):
    """`destination/s3` → `destination-s3.md`, the reference's and the guide notes' file name."""
    return plugin.replace("/", "-") + ".md"


def plugins():
    """Every first-party plugin, as (package, "kind/name")."""
    return [(package, p) for package, about in PACKAGES.items() for p in about["provides"]]


def describe_files(root=ROOT):
    """Every first-party plugin's `describe` reply, from its package's committed describe.json
    (written from the built plugins and checked by the `describe_json` test)."""
    out = {}
    for package, about in PACKAGES.items():
        path = root / about.get("go", f"plugins/{package}") / "describe.json"
        replies = json.loads(path.read_text())
        for plugin in about["provides"]:
            if plugin not in replies:
                raise SystemExit(f"{path} has no `{plugin}`: run the describe_json test with DRE_UPDATE_DESCRIBE=1")
            out[plugin] = replies[plugin]
    return out


def _cell(v):
    if v is None:
        return ""
    if isinstance(v, bool):
        return f"`{str(v).lower()}`"
    if isinstance(v, str):
        shown = v if v and v.isprintable() else json.dumps(v)
        return "`" + shown.replace("|", "\\|") + "`"
    if isinstance(v, list):
        return ", ".join(_cell(x) for x in v)
    return f"`{json.dumps(v)}`"


def _text(s):
    return (s or "").replace("|", "\\|").replace("\n", " ")


def _relink(md, page="plugins.md"):
    """Links relative to docs/ made absolute, so they work from inside an installed skill."""
    md = re.sub(r"\]\(#([^)]+)\)", r"](%s#\1)" % (DOCS_URL + page), md)
    return re.sub(r"\]\((?!https?:|#)([^)]+)\)", lambda m: f"]({DOCS_URL}{m.group(1)})", md)


def _demote(md, to):
    """Shift the section's headings so the shallowest becomes level `to`."""
    levels = [len(m) for m in re.findall(r"^(#{1,6}) ", md, re.M)]
    if not levels:
        return md
    shift = to - min(levels)
    return re.sub(r"^(#{1,6}) ", lambda m: "#" * min(6, len(m.group(1)) + shift) + " ", md, flags=re.M)


def reference(plugin, package, reply, docs, notes):
    kind, name = plugin.split("/")
    version = package_version(package)
    conn = reply.get("connection_fields") or []
    opts = reply.get("option_fields") or []
    out = [
        "---",
        f"plugin: {plugin}",
        f"package: {package}",
        f'generated_from: "{version}"',
        f'covers: "{covers(version)}"',
        "---",
        "",
        "<!-- Generated by .github/scripts/skills.py from the plugin's `describe` reply, docs/plugins.md and",
        f"     skills/shared/guide-notes/{reference_name(plugin)}. Edit those and regenerate; don't edit this file. -->",
        "",
        f"# The `{name}` {kind} (package `{package}`)",
        "",
        f"This reference covers `{package}` versions `{covers(version)}`. Compare with the version",
        "`dre plugin list` shows (in a project, `dre.lock` pins it); outside that range, say its",
        "options may differ from this list before relying on it.",
        "",
        "## Profile fields",
        "",
    ]
    if kind == "format":
        out += ["None: a format has no profile. Its options go in the report's `output:` block, or for",
                "the whole project under `format_options.<format>` in `dre_project.yml`.", ""]
    else:
        section = "sources" if kind == "source" else "destinations"
        out += [f"A `profiles.yml` target under `{section}:` with `type: {name}` takes these fields.",
                "Never write a secret's value: use `env_var()` (SEC-3).", "",
                "| Field | Required | Secret | Default | Description |", "|---|---|---|---|---|"]
        for f in conn:
            out.append(f"| `{f['name']}` | {'yes' if f.get('required') else 'no'} | "
                       f"{'yes' if f.get('secret') else 'no'} | {_cell(f.get('default'))} | {_text(f.get('description'))} |")
        out.append("")
    out += ["## Report options", ""]
    if kind == "source":
        out += ["None: a source's settings are its profile fields.", ""]
    elif not opts:
        out += ["None.", ""]
    else:
        where = "the report's `output:` block" if kind == "format" else "the report's `output.destination` entry"
        out += [f"Set in {where}.", "",
                "| Option | Type | Required | Default | Allowed | Description |", "|---|---|---|---|---|---|"]
        for f in opts:
            allowed = _cell(f.get("choices"))
            bounds = [f"{k} {f[k]}" for k in ("min", "max") if f.get(k) is not None]
            allowed = ", ".join([a for a in [allowed] if a] + bounds)
            out.append(f"| `{f['name']}` | {f.get('type', '')} | {'yes' if f.get('required') else 'no'} | "
                       f"{_cell(f.get('default'))} | {allowed} | {_text(f.get('description'))} |")
        out.append("")
    out += ["## From the plugin docs", ""]
    for entry in DOC_SECTIONS[plugin]:
        title, body, page = docs.section(entry)
        body = body.strip("\n")
        out += [f"### {title}", "", _relink(_demote(body, 4), page) if body else "", ""]
    out += ["## Guide notes", "", notes.strip(), ""]
    return "\n".join(out).rstrip("\n") + "\n"


# ---------------------------------------------------------------------------------------------
# Sync and generate


def inline_shared(text, snippets):
    def repl(m):
        name = m.group(1)
        if name not in snippets:
            raise KeyError(f"no skills/shared/{name}")
        body = snippets[name]
        return f"<!-- BEGIN shared/{name} -->\n{body.rstrip(chr(10))}\n<!-- END shared/{name} -->"

    return re.sub(r"<!-- BEGIN shared/(\S+) -->\n.*?<!-- END shared/\1 -->", repl, text, flags=re.S)


def skill_dirs(skills_root):
    return sorted(p.parent.name for p in skills_root.glob("*/SKILL.md"))


def sync(root):
    """Inline the shared snippets and copy the practices file, under `root` (a checkout)."""
    skills_root = root / "skills"
    snippets = {p.name: p.read_text() for p in (skills_root / "shared").glob("*.md")}
    # Without its previous/next links: the pages they point to aren't copied.
    practices = strip_nav((root / PRACTICES).read_text()).rstrip("\n") + "\n"
    for skill in skill_dirs(skills_root):
        path = skills_root / skill / "SKILL.md"
        path.write_text(inline_shared(path.read_text(), snippets))
        target = skills_root / skill / "references" / "practices.md"
        if "practices" in COPIES.get(skill, []):
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(practices)
        elif target.exists():
            target.unlink()


def generate(root, replies):
    """Write every plugin reference into each skill that takes them, under `root`."""
    skills_root = root / "skills"
    docs = Docs(root)
    files = {}
    for package, plugin in plugins():
        notes_path = skills_root / "shared" / "guide-notes" / reference_name(plugin)
        notes = notes_path.read_text() if notes_path.exists() else "None yet."
        files[reference_name(plugin)] = reference(plugin, package, replies[plugin], docs, notes)
    for skill in skill_dirs(skills_root):
        folder = skills_root / skill / "references" / "plugins"
        if folder.exists():
            shutil.rmtree(folder)
        if "plugins" in COPIES.get(skill, []):
            folder.mkdir(parents=True)
            for fname, text in files.items():
                (folder / fname).write_text(text)


def diff_tree(a, b):
    """Relative paths of files that differ between two folders, or exist in only one."""
    def files(root):
        return {p.relative_to(root).as_posix(): p for p in root.rglob("*") if p.is_file() and p.name != ".DS_Store"}

    fa, fb = files(a), files(b)
    return [rel for rel in sorted(set(fa) | set(fb))
            if rel not in fa or rel not in fb or fa[rel].read_bytes() != fb[rel].read_bytes()]


# ---------------------------------------------------------------------------------------------
# Checks


def skills_version(root):
    return json.loads((root / "skills/.claude-plugin/plugin.json").read_text())["version"]


def check(root, replies):
    problems = []
    skills_root = root / "skills"
    version = skills_version(root)
    parse_version(version)

    # Frontmatter and versions.
    names = skill_dirs(skills_root)
    if sorted(COPIES) != names:
        problems.append(f"COPIES in skills.py lists {sorted(COPIES)}, but the skills are {names}")
    ranges = set()
    for skill in names:
        try:
            fm, _ = frontmatter((skills_root / skill / "SKILL.md").read_text())
        except ValueError as e:
            problems.append(f"skills/{skill}/SKILL.md: {e}")
            continue
        problems += [f"skills/{skill}/SKILL.md: {p}" for p in frontmatter_problems(skill, fm)]
        meta = fm.get("metadata") if isinstance(fm.get("metadata"), dict) else {}
        ranges.add(meta.get("dre"))
        if meta.get("version") != version:
            problems.append(f"skills/{skill}/SKILL.md: `metadata.version` must be the skills version, {version}")
        try:
            rng = parse_range(meta.get("dre", ""))
            if not in_range(workspace_version(), rng):
                problems.append(f"skills/{skill}/SKILL.md: `metadata.dre` ({meta['dre']}) doesn't include this "
                                f"checkout's dre, {workspace_version()}")
        except ValueError as e:
            problems.append(f"skills/{skill}/SKILL.md: `metadata.dre`: {e}")
    if len(ranges) > 1:
        problems.append(f"the skills state different `metadata.dre` ranges: {sorted(map(str, ranges))}; they're released together")
    market = json.loads((root / ".claude-plugin/marketplace.json").read_text())
    entry = next((p for p in market.get("plugins", []) if p.get("name") == "dre"), None)
    src = (entry or {}).get("source", {})
    if not entry or (src.get("source"), src.get("path"), src.get("ref")) != ("git-subdir", "skills", "skills-latest"):
        problems.append(".claude-plugin/marketplace.json: the `dre` entry must be a git-subdir source at `skills` "
                        "on `skills-latest`, so the channel serves releases, never master")

    # Commands, practices and plugin names in every Markdown file the skills ship.
    practices = defined_practices((root / PRACTICES).read_text())
    # Core's own: the `local` destination and the `message` format.
    known = {p.split("/")[1] for _, p in plugins()} | {"local", "message"}
    cli = ReferenceCli((root / "docs/cli-reference.md").read_text())
    for path in sorted(skills_root.rglob("*.md")):
        rel = path.relative_to(root).as_posix()
        md = path.read_text()
        problems += command_problems(cli, rel, md)
        for pid in sorted(cited_practices(md) - practices):
            problems.append(f"{rel}: cites {pid}, which isn't in {PRACTICES}")
        # Hand-written files only: the plugin docs use `type:` for other things (fixed-width columns).
        if "references/plugins/" not in rel:
            problems += [f"{rel}: {u} names no first-party plugin" for u in unknown_plugins(md, known)]
    # The user docs, which the skills point to, name only real commands too.
    for path in [root / "README.md", *sorted((root / "docs").glob("*.md"))]:
        problems += command_problems(cli, path.relative_to(root).as_posix(), path.read_text())

    # Every declared field and option documented.
    docs = Docs(root)
    for _, plugin in plugins():
        text = "".join(docs.section(e)[1] for e in DOC_SECTIONS[plugin])
        fields = (replies[plugin].get("connection_fields") or []) + (replies[plugin].get("option_fields") or [])
        problems += undocumented(plugin, fields, text)
        if not (skills_root / "shared" / "guide-notes" / reference_name(plugin)).exists():
            problems.append(f"skills/shared/guide-notes/{reference_name(plugin)} is missing")

    # Copies and generated files up to date.
    with tempfile.TemporaryDirectory() as tmp:
        fresh = pathlib.Path(tmp)
        shutil.copytree(skills_root, fresh / "skills")
        (fresh / "docs").mkdir()
        for doc in (PLUGIN_DOCS, PRACTICES, *(f"docs/plugin-{p}.md" for p in {e[1] for es in DOC_SECTIONS.values() for e in es if e[0] == "page"})):
            shutil.copy(root / doc, fresh / doc)
        sync(fresh)
        generate(fresh, replies)
        for rel in diff_tree(skills_root, fresh / "skills"):
            problems.append(f"skills/{rel} is out of date: run `python3 .github/scripts/skills.py sync` and "
                            "`generate`")
    return problems


def release_check(root, tag):
    tagged = tag_version(tag)
    if not tagged:
        return [f"tag {tag}: expected skills-v<version>"]
    version = skills_version(root)
    if tagged != version:
        return [f"tag {tag} is version {tagged}, but skills/.claude-plugin/plugin.json says {version}"]
    return []


def tag_version(tag):
    """`skills-v1.2.0` → `1.2.0`; None for any other tag."""
    m = re.fullmatch(r"skills-v(.+)", tag)
    return m.group(1) if m else None


def serves_latest(tag, tags):
    """Whether releasing `tag` moves `skills-latest` (what the install channels serve) to it: the
    newest stable release does; a pre-release only while no stable skills release exists."""
    versions = [v for v in map(tag_version, tags) if v]
    version = tag_version(tag)
    stable = [v for v in versions if "-" not in v]
    pool = stable if stable else versions
    return version in pool and parse_version(version) == max(map(parse_version, pool))


def release_notes(root, tag):
    version = tag_version(tag)
    fm, _ = frontmatter((root / "skills/dre/SKILL.md").read_text())
    return "\n".join([
        f"DRE's agent skills, {version}. Supported dre: `{fm['metadata']['dre']}`.",
        "",
        "Install or update:",
        "- Claude Code: `claude plugin marketplace add get-dre/dre`, then `claude plugin install dre@dre`"
        " (to update: `claude plugin marketplace update dre`, then `claude plugin update dre@dre`).",
        f"- Other agents (Codex, Cursor, GitHub Copilot, Gemini CLI, ...): `npx skills add get-dre/dre#{tag}`"
        " for this release, or `#skills-latest` for the newest.",
        "",
        "See [skills/README.md](https://github.com/get-dre/dre/blob/master/skills/README.md).",
    ])


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("sync")
    sub.add_parser("generate")
    sub.add_parser("check")
    r = sub.add_parser("release-check")
    r.add_argument("tag")
    n = sub.add_parser("release-notes")
    n.add_argument("tag")
    s = sub.add_parser("serves-latest", help="exit 0 if releasing TAG should move skills-latest")
    s.add_argument("tag")
    a = ap.parse_args()
    if a.cmd == "release-notes":
        print(release_notes(ROOT, a.tag))
    elif a.cmd == "serves-latest":
        tags = subprocess.run(["git", "tag", "-l", "skills-v*"], cwd=ROOT, capture_output=True, text=True,
                              check=True).stdout.split()
        sys.exit(0 if serves_latest(a.tag, tags) else 1)
    elif a.cmd == "sync":
        sync(ROOT)
    elif a.cmd == "generate":
        generate(ROOT, describe_files())
    else:
        problems = (check(ROOT, describe_files()) if a.cmd == "check"
                    else release_check(ROOT, a.tag))
        for p in problems:
            print(f"::error::{p}" if "GITHUB_ACTIONS" in os.environ else p)
        if problems:
            sys.exit(f"{len(problems)} problem(s)")
        print("ok")


if __name__ == "__main__":
    main()
