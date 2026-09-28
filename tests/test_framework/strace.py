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
# Calls that use a descriptor someone else handed out. Only a failure in one of
# these says anything about descriptor lifetime; a failing `openat` just means
# the path was not there, and it carries no fd to trace.
USING = ("lseek", "_llseek", "read", "pread64", "close")
# A process that closes more descriptors than any real workload holds is a
# forked child clearing its inherited table before exec. Its closes happen in
# its own copy of the table and cannot affect the parent, so reporting them as
# "another thread closed your descriptor" would be exactly backwards. The
# threshold is a heuristic, named as one.
FORK_CLOSE_STORM = 4096


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
            if event["ret"].startswith("-1")
            and event["name"] in USING
            and marker in event["args"]
        ),
        None,
    )
    if failure is None:
        return (
            f"({len(events)} calls traced in {path}; no lseek/read/close on "
            f"{marker} failed, so either the fault did not occur here or the "
            "failing call was outside the traced window)"
        )

    fd = _fd_of(failure)
    children = _forked_children(events)
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
        if event["tid"] == failure["tid"]:
            mark = " "
        elif event["tid"] in children:
            mark = "c"
        else:
            mark = "*"
        lines.append(
            f"  {mark} {event['time']} tid {event['tid']:>7} "
            f"{event['name']} = {event['ret']}"
        )
    siblings = sorted(
        {
            e["tid"]
            for e in history
            if e["tid"] != failure["tid"] and e["tid"] not in children
        }
    )
    if siblings:
        lines.append(
            f"* another thread of this process touched fd {fd} before it "
            f"failed: {', '.join(siblings)}"
        )
    else:
        lines.append(
            f"no sibling thread touched fd {fd}: its history is the failing "
            "thread's alone"
        )
    if any(e["tid"] in children for e in history):
        lines.append(
            "c = a forked child clearing its inherited descriptor table before "
            "exec; those closes are in its own copy and cannot affect this "
            "process"
        )
    return "\n".join(lines)


def _forked_children(events):
    """PIDs that closed their whole descriptor table, i.e. forked children.

    strace's first column is a PID, and a fork and a thread look alike there.
    A child clearing thousands of inherited descriptors is unmistakable, and
    failing to exclude it turns an ordinary fork into a false report of one
    thread destroying another's descriptor.
    """
    closes = {}
    for event in events:
        if event["name"] == "close":
            closes[event["tid"]] = closes.get(event["tid"], 0) + 1
    return {tid for tid, count in closes.items() if count >= FORK_CLOSE_STORM}
