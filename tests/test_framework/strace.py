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
# With -f, a call that blocks is split across two lines. The first carries the
# arguments, the second the result, and neither is usable alone: in run
# 36382915647 the open that names the block file was the `<unfinished ...>`
# half and its `resumed` half returned a bare `= 27`.
UNFINISHED = re.compile(r"^(?P<name>\w+)\((?P<args>.*?)\s*<unfinished \.\.\.>$")
RESUMED = re.compile(
    r"^<\.\.\.\s+(?P<name>\w+)\s+resumed>(?P<rest>.*?)\)\s+=\s+(?P<ret>.+?)"
    r"(?:\s+<[\d.]+>)?$"
)
# A descriptor as strace's `-y` prints it: `7</path/to/blk00000.dat>`.
FD = re.compile(r"(-?\d+)(?:<(?P<path>[^>]*)>)?")
ANNOTATED_FD = re.compile(r"(\d+)<([^>]*)>")
# The path argument of an open, as strace quotes it.
FILENAME = re.compile(r'"((?:[^"\\]|\\.)*)"')

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
# How far back to show a descriptor's history. Long enough to hold the calls
# that raced, short enough to exclude the number's previous tenants.
HISTORY_SECONDS = 0.5


def parse_events(path):
    """Every completed call in the trace, oldest first.

    A call split by -f is reported once, at the time it completed, with the
    arguments from its `<unfinished ...>` half.
    """
    events = []
    pending = {}
    with open(path, "r", errors="replace") as handle:
        for line in handle:
            line = line.rstrip("\n")
            head = LINE.match(line)
            if head is None:
                continue
            tid, rest = head.group("tid"), head.group("rest")

            split = UNFINISHED.match(rest)
            if split is not None:
                pending[tid] = (
                    split.group("name"),
                    split.group("args"),
                    line,
                    head.group("time"),
                )
                continue

            resumed = RESUMED.match(rest)
            if resumed is not None:
                name, args, opening, started = pending.pop(
                    tid, (resumed.group("name"), "", "", head.group("time"))
                )
                events.append(
                    {
                        "tid": tid,
                        # A close takes effect while the call is running, so a
                        # call that started before another thread's failure can
                        # be its cause even though it completed afterwards.
                        "started": started,
                        "time": head.group("time"),
                        "name": name,
                        "args": args + resumed.group("rest"),
                        "ret": resumed.group("ret").strip(),
                        "raw": f"{opening}\n    {line}" if opening else line,
                    }
                )
                continue

            body = CALL.match(rest)
            if body is None:
                continue  # `+++ exited +++`, a signal
            events.append(
                {
                    "tid": tid,
                    "started": head.group("time"),
                    "time": head.group("time"),
                    "name": body.group("name"),
                    "args": body.group("args"),
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


def _opened_fds(event):
    """The descriptors an opening call handed out, with their paths.

    `pipe2` reports both ends in its arguments rather than its return value,
    and both ends matter: the descriptor that killed a block-file handle in run
    36382915647 was one end of a pipe.
    """
    if event["name"].startswith("pipe"):
        return ANNOTATED_FD.findall(event["args"])
    match = FD.match(event["ret"].strip())
    if match is None or match.group(1).startswith("-"):
        return []
    path = match.group("path")
    if not path and event["name"] in ("openat", "open"):
        # strace could not resolve the descriptor - which is what happens when
        # it has already been closed underneath the caller, the very case this
        # exists for. The filename is still in the call's own arguments.
        quoted = FILENAME.search(event["args"])
        path = quoted.group(1) if quoted else ""
    return [(match.group(1), path or "")]


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

    # strace annotates a descriptor with `-y` by resolving it through /proc at
    # the time of the call. A descriptor that has just been closed underneath
    # its owner cannot be resolved, so the failing call prints a bare number:
    # in run 36382915647 the fault was `lseek(27, 8192, SEEK_SET) = -1 EBADF`,
    # with nothing naming the block file. Matching on the path alone therefore
    # misses exactly the failure this exists to find. Track what each
    # descriptor was opened as instead, and match on that.
    opened = {}
    failure = None
    children = _forked_children(events)
    for event in events:
        fd = _fd_of(event)
        if event["name"] in OPENING and not event["ret"].startswith("-1"):
            for number, opened_path in _opened_fds(event):
                opened[number] = opened_path
            continue
        if (
            event["ret"].startswith("-1")
            and event["name"] in USING
            and fd is not None
            and not fd.startswith("-")
            # A forked child failing to close an inherited descriptor is its
            # own business and says nothing about this process.
            and event["tid"] not in children
            and (marker in event["args"] or marker in opened.get(fd, ""))
        ):
            failure = event
            break
        if event["name"] == "close" and not event["ret"].startswith("-1"):
            opened.pop(fd, None)
    if failure is None:
        return (
            f"({len(events)} calls traced in {path}; no lseek/read/close on "
            f"{marker} failed, so either the fault did not occur here or the "
            "failing call was outside the traced window)"
        )

    fd = _fd_of(failure)
    # A short window before the failure, ordered by when each call started.
    # Not "everything since the last open": the close that destroyed the
    # descriptor in run 36382915647 *completed before* the open that handed the
    # number out, because the kernel reuses a number the instant it is free.
    # Trimming at the last open hides precisely the call that matters.
    failed_at = _seconds(failure["time"])
    history = sorted(
        (
            event
            for event in events
            if _fd_of(event) == fd
            and failed_at - HISTORY_SECONDS <= _seconds(event["started"]) <= failed_at
        ),
        key=lambda event: _seconds(event["started"]),
    )[-20:]
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
        when = event["started"]
        if event["time"] != when:
            when = f"{when}->{event['time'][-9:]}"
        lines.append(
            f"  {mark} {when} tid {event['tid']:>7} "
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


def _seconds(clock):
    """`HH:MM:SS.ffffff` as seconds, for ordering and windowing."""
    try:
        hours, minutes, rest = clock.split(":")
        return int(hours) * 3600 + int(minutes) * 60 + float(rest)
    except (ValueError, AttributeError):
        return 0.0


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
