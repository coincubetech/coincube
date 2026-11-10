"""Private, crash-safe checkpoints. A checkpoint is evidence, never authorization."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import time


class Blocked(Exception):
    def __init__(self, code, message):
        self.code = code
        super().__init__(message)


class Failed(Blocked):
    pass


def private_read(path):
    path = Path(path)
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except OSError:
        raise Blocked("PRIVATE_FILE_UNAVAILABLE", "A required private fixture or credential file is unavailable or is a symlink.") from None
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise Blocked("PRIVATE_FILE", "Fixture files must be owned by this user with mode 600.")
        data = stream.read(4_000_001)
        if len(data) > 4_000_000:
            raise Failed("FILE_SIZE", "Fixture file exceeds the size limit.")
        return data


def atomic_write(path, data):
    path = Path(path)
    fd, tmp = tempfile.mkstemp(prefix=".qa-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(tmp, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


class Journal:
    def __init__(self, directory, binding):
        self.directory = Path(directory).absolute()
        if self.directory.is_symlink():
            raise Blocked("RUN_DIRECTORY", "Run directory cannot be a symlink.")
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = self.directory.stat()
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise Blocked("RUN_DIRECTORY", "Run directory must be owned by this user with mode 700.")
        self.lock = os.open(self.directory / "run.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(self.lock)
            raise Blocked("RUN_BUSY", "Another process owns this run directory.") from None
        self.path = self.directory / "checkpoint.json"
        try:
            if self.path.exists():
                self.data = json.loads(private_read(self.path))
                if self.data.get("version") != 1 or self.data.get("binding") != digest(binding):
                    raise Failed("RUN_MISMATCH", "Configuration or fixture changed; use a new run directory.")
            else:
                self.data = {"version": 1, "binding": digest(binding), "artifacts": {}, "stages": {}}
                self.save()
        except BaseException:
            self.close()
            raise

    def save(self):
        atomic_write(self.path, (json.dumps(self.data, indent=2, sort_keys=True) + "\n").encode())

    def artifact(self, name, value):
        self.data["artifacts"][name] = value
        self.save()

    def outcome(self, stage, status, code=""):
        self.data["stages"][stage] = {"status": status, "code": code, "at": int(time.time())}
        self.save()

    def close(self):
        if self.lock is not None:
            os.close(self.lock)
            self.lock = None
