"""Turns a cargo build/test log into GitHub workflow annotations.

Usage: ci_annotate.py <log> <cargo exit status>

Annotations are capped per step (about 10 errors and 10 warnings), so the
most useful ones go first: compiler errors with their location and full
message, then failed tests with their output, then the end of the log if
nothing else explained the failure. Warnings never fail anything.
"""

import re
import sys

MAX_LINES = 40
CAP = 10

HEAD = re.compile(r"^(error|warning)(\[[A-Za-z0-9]+\])?: (.*)$")
LOCATION = re.compile(r"^\s*--> (.+?):(\d+):(\d+)$")
TOOL_ERROR = re.compile(r"error C\d+|error MSB\d+|error LNK\d+|fatal error|: error|CMake Error|Could NOT find|ninja: build stopped")
TEST_OUT =re.compile(r"^---- (.+?) stdout ----$")
# Summary lines that carry nothing of their own.
NOISE = re.compile(r"^(`.*` \(.*\) generated|`.*` generated|could not compile|aborting due to|build failed|\d+ warnings? emitted)")


def esc_data(s):
    return s.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def esc_prop(s):
    return esc_data(s).replace(":", "%3A").replace(",", "%2C")


def emit(kind, message, title=None, file=None, line=None):
    props = []
    if file:
        props.append("file=" + esc_prop(file.replace("\\", "/")))
    if line:
        props.append("line=" + esc_prop(str(line)))
    if title:
        props.append("title=" + esc_prop(title[:200]))
    head = "::%s %s::" % (kind, ",".join(props)) if props else "::%s::" % kind
    print(head + esc_data(message))


def compiler_blocks(lines):
    """(kind, title, file, line, text) per rustc/cargo diagnostic."""
    out = []
    i = 0
    while i < len(lines):
        m = HEAD.match(lines[i])
        if not m or NOISE.match(m.group(3)):
            i += 1
            continue
        block = [lines[i]]
        j = i + 1
        if m.group(3).startswith("failed to run custom build command"):
            # A build script's whole output follows, blank lines and all;
            # its end (stderr, the panic) says what went wrong.
            while j < len(lines) and not HEAD.match(lines[j]):
                j += 1
            body = [l for l in lines[i + 1 : j] if l.strip() and "cargo:rerun-if" not in l and not re.match(r"^\s*\w+ = (None|Some)", l)]
            # The compiler's own errors (MSVC, MSBuild, CMake, gcc/clang)
            # are buried in the middle: one annotation of them first.
            errs = []
            for l in body:
                if TOOL_ERROR.search(l) and l.strip() not in errs:
                    errs.append(l.strip())
            if errs:
                out.append((m.group(1), "compiler errors in " + m.group(3).split("for ")[-1], None, None, "\n".join(errs[:MAX_LINES])))
            block += body[-(MAX_LINES - 1) :]
            out.append((m.group(1), m.group(3), None, None, "\n".join(block)))
            i = j
            continue
        # A diagnostic runs to the next blank line, capped.
        while j < len(lines) and lines[j].strip() != "" and not HEAD.match(lines[j]) and len(block) < MAX_LINES:
            block.append(lines[j])
            j += 1
        file = line = None
        for b in block[1:4]:
            loc = LOCATION.match(b)
            if loc:
                file, line = loc.group(1), loc.group(2)
                break
        out.append((m.group(1), m.group(3), file, line, "\n".join(block)))
        i = j
    return out


def failed_tests(lines):
    out = []
    i = 0
    while i < len(lines):
        m = TEST_OUT.match(lines[i])
        if not m:
            i += 1
            continue
        block = []
        j = i + 1
        while j < len(lines) and not TEST_OUT.match(lines[j]) and lines[j].strip() != "failures:" and len(block) < MAX_LINES:
            block.append(lines[j])
            j += 1
        out.append((m.group(1), "\n".join(block).strip()))
        i = j
    return out


def main():
    with open(sys.argv[1], encoding="utf-8", errors="replace") as f:
        lines = [l.rstrip("\r\n") for l in f]
    failed = len(sys.argv) > 2 and sys.argv[2] != "0"

    diags = compiler_blocks(lines)
    errors = [d for d in diags if d[0] == "error"]
    warnings = [d for d in diags if d[0] == "warning"]
    tests = failed_tests(lines)

    # Errors with a location first: they say exactly what to fix.
    errors.sort(key=lambda d: d[2] is None)
    shown = 0
    for _, title, file, line, text in errors[: CAP - 1 if tests else CAP]:
        emit("error", text, title=title, file=file, line=line)
        shown += 1
    for name, text in tests[: max(CAP - shown, 1)]:
        emit("error", text or "(no output)", title="test failed: " + name)
    if tests:
        emit("notice", "failed tests: " + ", ".join(n for n, _ in tests))
    if failed and not errors and not tests:
        emit("error", "\n".join(lines[-MAX_LINES:]), title="cargo failed (end of log)")

    seen = set()
    for _, title, file, line, text in warnings:
        key = (title, file, line)
        if key in seen:
            continue
        seen.add(key)
        emit("warning", text, title=title, file=file, line=line)
        if len(seen) >= CAP:
            break


if __name__ == "__main__":
    main()
