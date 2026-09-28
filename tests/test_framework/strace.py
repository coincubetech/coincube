"""Read an `strace -f -tt -T -y` log and explain one failing call (#394).

A trace of a loaded bitcoind is tens of megabytes of syscalls that all
succeeded. The three lines that matter are the failing one and the two that
created and destroyed the descriptor it was using, and those are the ones the
failure message should carry — downloading an artifact to find them is what
made #394 take three weeks to characterise.
"""

import re

# `12345 05:22:56.123456 openat(AT_FDCWD</cwd>, "/d/blk00000.dat", O_RDONLY) = 7</d/blk00000.dat> <0.000012>`
LINE = re.compile(r"^(?P<tid>\d+)\s+(?P<time>\d\d:\d\d:\d\d\.\d+)\s+(?P<rest>.*)$")
CALL = re.compile(r"^(?P<name>\w+)\((?P<args>.*?)\)\s+=\s+(?P<ret>.+?)(?:\s+<[\d.]+>)?$")
RESUMED = re.compile(
    r"^<\.\.\.\s+(?P<name>\w+)\s+resumed>.*?\)\s+=\s+(?P<ret>.+?)(?:\s+<[\d.]+>)?$"
)
# A descriptor as strace's `-y` prints it: `7</path/to/blk00000.dat>`.
FD = re.compile(r"(-?\d+)(?:<(?P<path>[^>]*)>)?")

# Calls that hand out a descriptor, so the return value is the fd rather than
# the first argument.
OPENING = ("openat", "open", "dup", "dup2", "dup3", "socket", "accept", "accept4")


def parse_events(path):
    """Every complete call in the trace, oldest first."""
    events = []
    with open(path, "r", errors="replace") as handle:
        for line in handle:
            line = line.rstrip("\n")
            head = LINE.match(line)
            if head is None:
                continue
            body = CALL.match(head.group("rest")) or RESUMED.match(head.group("rest"))
            if body is None:
                continue  # `<unfinished ...>`, `+++ exited +++`, a signal
            events.append(
                {
                    "tid": head.group("tid"),
                    "time": head.group("time"),
                    "name": body.group("name"),
                    "args": body.groupdict().get("args", ""),
                    "ret": body.group("ret").strip(),
                    "raw": line,
                }
            )
    return events


def _fd_of(event):
    """The descriptor an event is about: its result when it opened one, else
    its first argument."""
    source = event["ret"] if event["name"] in OPENING else event["args"]
    match = FD.match(source.strip())
    return match.group(1) if match else None


def describe_failure(path, marker):
    """The first failing call naming `marker`, and that descriptor's history.

    Returns a printable report, or a line saying why there is nothing to show.
    Never raises on a malformed trace: this runs inside a failure path, and a
    parser that throws there would hide the failure it was meant to explain.
    """
    try:
        events = parse_events(path)
    except OSError as error:
        return f"(could not read {path}: {error})"
    if not events:
        return f"({path} holds no parsable strace lines)"

    failure = next(
        (
            event
            for event in events
            if event["ret"].startswith("-1") and marker in event["args"]
        ),
        None,
    )
    if failure is None:
        return (
            f"({len(events)} calls traced in {path}; none of them failed on "
            f"{marker}, so the failing call was made by an untraced process or "
            "outside the traced window)"
        )

    fd = _fd_of(failure)
    history = [
        event
        for event in events
        if _fd_of(event) == fd and event["time"] <= failure["time"]
    ]
    # Anything that touched this descriptor from another thread is the whole
    # question, so mark it rather than making a reader diff the TIDs.
    lines = [
        f"{len(events)} calls traced; fd {fd} failed in tid {failure['tid']} "
        f"at {failure['time']}:",
        f"    {failure['raw']}",
        f"history of fd {fd} (* = a different thread from the one that failed):",
    ]
    for event in history[-40:]:
        mark = " " if event["tid"] == failure["tid"] else "*"
        lines.append(
            f"  {mark} {event['time']} tid {event['tid']:>7} "
            f"{event['name']} = {event['ret']}"
        )
    other = sorted({e["tid"] for e in history if e["tid"] != failure["tid"]})
    if other:
        lines.append(
            f"other threads touched fd {fd} before it failed: {', '.join(other)}"
        )
    else:
        lines.append(
            f"no other thread touched fd {fd}: its history is the failing "
            "thread's alone"
        )
    return "\n".join(lines)
