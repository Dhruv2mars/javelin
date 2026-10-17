"""Disposable ordinary-path workers. The Bun supervisor owns process deadlines."""
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import threading
import time


def emit(value):
    print(json.dumps(value, ensure_ascii=True), flush=True)


def observe(fn):
    try:
        return {"ok": True, "value": fn()}
    except OSError as error:
        return {"ok": False, "errno": error.errno, "error": str(error)}


def tool(argv, timeout=35):
    start = time.monotonic()
    try:
        r = subprocess.run(argv, capture_output=True, timeout=timeout)
        result = {"argv": argv, "exit": r.returncode,
                  "stdout": r.stdout.decode(errors="replace"), "stderr": r.stderr.decode(errors="replace")}
    except subprocess.TimeoutExpired as error:
        result = {"argv": argv, "exit": None, "timeout": True,
                  "stdout": (error.stdout or b"").decode(errors="replace"),
                  "stderr": (error.stderr or b"").decode(errors="replace")}
    result["elapsedSeconds"] = time.monotonic() - start
    emit({"command": result})
    return result


def usage(path):
    files = list(Path(path).rglob("*"))
    stats = [p.lstat() for p in files if p.is_file()]
    return {"files": len(stats), "logicalBytes": sum(s.st_size for s in stats),
            "allocatedBytes": sum(s.st_blocks * 512 for s in stats)}


def fixture(base):
    base.mkdir(parents=True)
    (base / "CaseName.txt").write_text("case-secret\n")
    (base / "caf\u00e9.txt").write_text("unicode-secret\n")
    (base / "held.txt").write_bytes(b"held-base\n" + bytes(131072) + b"unread-tail\n")
    (base / "unrelated.txt").write_text("distinct-control\n")
    (base / "owner-secret").write_text("owner-only-secret\n")
    (base / "owner-secret").chmod(0o600)
    for folder in ["paged", "watch", "cost-32", "cost-512", "cost-2048"]:
        (base / folder).mkdir()
    for n in range(640):
        (base / "paged" / (f"entry-{n:05}-" + "x" * 100)).write_text(f"{n}\n")
    for count in [32, 512, 2048]:
        for n in range(count):
            (base / f"cost-{count}" / f"unit{n:05}.c").write_text(f"int value_{n}(void) {{ return {n}; }}\n")
    emit({"fixture": usage(base), "pagedEntries": 640, "tiers": [32, 512, 2048],
          "ceilingEntries": 50000, "ceilingBytes": 1073741824,
          "manifestSha256": hashlib.sha256(b"".join(
              str(p.relative_to(base)).encode() + b"\0" + p.read_bytes()
              for p in sorted(base.rglob("*")) if p.is_file())).hexdigest()})


def aliases(base, mount):
    results = []
    for spelling, alias in [("CaseName.txt", "casename.txt"), ("caf\u00e9.txt", "cafe\u0301.txt")]:
        a, b = base / spelling, base / alias
        control = observe(lambda: {"sameInode": a.stat().st_ino == b.stat().st_ino,
                                   "aliasContents": b.read_text()})
        before = tool(["/bin/cat", str(mount / alias)])
        removal = tool(["/bin/rm", str(mount / spelling)])
        exact = tool(["/bin/cat", str(mount / spelling)])
        aliased = tool(["/bin/cat", str(mount / alias)])
        results.append({"spelling": spelling, "alias": alias,
                        "filenameHex": [os.fsencode(n).hex() for n in [spelling, alias]], "control": control,
                        "before": before, "remove": removal, "exact": exact,
                        "aliasRead": aliased, "whiteoutBypass": removal["exit"] == 0 and aliased["exit"] == 0})
    emit({"aliases": results, "unrelated": tool(["/bin/cat", str(mount / "unrelated.txt")])})


def held(mount):
    path = mount / "acknowledged.txt"
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
    os.write(fd, b"completed-fsynced-write\n")
    synced = observe(lambda: os.fsync(fd))
    basefd = os.open(mount / "held.txt", os.O_RDONLY)
    dirfd = os.open(mount, os.O_RDONLY)
    emit({"ready": True, "pid": os.getpid(), "uid": os.getuid(),
          "fd": fd, "baseFd": basefd, "inode": os.fstat(fd).st_ino,
          "baseInode": os.fstat(basefd).st_ino, "rootInode": os.fstat(dirfd).st_ino,
          "acknowledged": "completed-fsynced-write\n", "fsyncCompleted": synced["ok"], "fsync": synced})
    if not select.select([sys.stdin], [], [], 70)[0]:
        raise TimeoutError("restart instruction deadline")
    if sys.stdin.readline().strip() != "after":
        raise ValueError("expected restart observation")
    def fdread(handle):
        os.lseek(handle, 131082 if handle == basefd else 0, os.SEEK_SET)
        return {"contents": os.read(handle, 128).decode(), "inode": os.fstat(handle).st_ino}
    def fdwrite():
        os.lseek(fd, 0, os.SEEK_END)
        written = os.write(fd, b"after-restart\n")
        os.fsync(fd)
        return {"written": written, "fsynced": True}
    emit({"sameMountAfterRestart": True, "heldWriteFd": observe(lambda: fdread(fd)),
          "heldBaseFd": observe(lambda: fdread(basefd)),
          "postRestartWrite": observe(fdwrite), "heldDirectory": observe(lambda: os.fstat(dirfd).st_ino),
          "newOpen": observe(lambda: path.read_text()),
          "root": observe(lambda: mount.stat().st_ino)})
    os.close(fd)
    os.close(basefd)
    os.close(dirfd)


# Minimal bounded RFC 5531/1813 client. This is a wire probe, never a filesystem implementation.
def u32(value): return struct.pack(">I", value)
def u64(value): return struct.pack(">Q", value)
def opaque(value): return u32(len(value)) + value + bytes((-len(value)) % 4)


class XDR:
    def __init__(self, data): self.data, self.pos = data, 0
    def take(self, size):
        if self.pos + size > len(self.data): raise ValueError("short XDR reply")
        value = self.data[self.pos:self.pos + size]
        self.pos += size
        return value
    def int(self): return struct.unpack(">I", self.take(4))[0]
    def long(self): return struct.unpack(">Q", self.take(8))[0]
    def blob(self):
        size = self.int()
        if size > 1048576: raise ValueError("oversized XDR blob")
        data = self.take(size)
        self.take((-size) % 4)
        return data
    def attr(self):
        if self.int(): self.take(84)


class RPC:
    def __init__(self, port): self.port, self.xid = port, 100
    def call(self, program, procedure, body):
        self.xid += 1
        auth = u32(1) + opaque(u32(1) + opaque(b"suitability") + u32(os.getuid()) + u32(os.getgid()) + u32(0))
        msg = b"".join(map(u32, [self.xid, 0, 2, program, 3, procedure])) + auth + u32(0) + u32(0) + body
        with socket.create_connection(("127.0.0.1", self.port), timeout=3) as sock:
            sock.settimeout(3)
            sock.sendall(u32(0x80000000 | len(msg)) + msg)
            def receive(n):
                data = b""
                while len(data) < n:
                    part = sock.recv(n - len(data))
                    if not part: raise OSError("RPC socket closed")
                    data += part
                return data
            reply = b""
            for _ in range(32):
                header = struct.unpack(">I", receive(4))[0]
                length = header & 0x7fffffff
                if length > 1048576: raise ValueError("oversized RPC fragment")
                reply += receive(length)
                if header & 0x80000000: break
            else: raise ValueError("too many fragments")
        emit({"rpc": {"uid": os.getuid(), "gid": os.getgid(), "program": program,
                       "procedure": procedure, "xid": self.xid,
                       "requestHex": msg.hex(), "replyHex": reply.hex()}})
        x = XDR(reply)
        if [x.int(), x.int(), x.int()] != [self.xid, 1, 0]: raise ValueError("RPC denied/mismatched")
        x.int(); x.blob()
        if x.int() != 0: raise ValueError("RPC accept error")
        return x
    def mount(self):
        x = self.call(100005, 1, opaque(b"/"))
        status = x.int()
        if status: raise OSError(f"mount status {status}")
        return x.blob()
    def lookup(self, fh, name):
        x = self.call(100003, 3, opaque(fh) + opaque(name.encode()))
        status = x.int()
        if status: raise OSError(f"lookup status {status}")
        return x.blob()
    def page(self, fh, cookie=0, verifier=bytes(8)):
        x = self.call(100003, 17, opaque(fh) + u64(cookie) + verifier + u32(1024) + u32(4096))
        status = x.int(); x.attr()
        if status: return {"status": status, "entries": []}
        verifier = x.take(8)
        entries = []
        while x.int():
            ident, name, cookie = x.long(), x.blob().decode(), x.long()
            x.attr()
            if x.int(): x.blob()
            entries.append({"fileid": ident, "name": name, "cookie": cookie})
        return {"status": status, "entries": entries, "eof": bool(x.int()), "verifier": verifier.hex()}


def pagination(port, mount):
    rpc = RPC(port)
    fh = rpc.lookup(rpc.mount(), "paged")
    first = rpc.page(fh)
    if not first["entries"] or first.get("eof"): raise ValueError("fixture did not cross pages")
    last = first["entries"][-1]
    second = rpc.page(fh, last["cookie"], bytes.fromhex(first["verifier"]))
    removal = tool(["/bin/rm", str(mount / "paged" / last["name"])])
    after_delete = rpc.page(fh, last["cookie"], bytes.fromhex(first["verifier"]))
    # Independently use a fresh page, rename its resume entry and retry that cookie.
    fresh = rpc.page(fh); last2 = fresh["entries"][-1]
    rename = tool(["/bin/mv", str(mount / "paged" / last2["name"]), str(mount / "paged" / "renamed-resume")])
    after_rename = rpc.page(fh, last2["cookie"], bytes.fromhex(fresh["verifier"]))
    # Real traversal/removal with visible errors. No retry or quiet fallback.
    mutations = []
    def mutate():
        for n in range(16, 48):
            old = mount / "paged" / (f"entry-{n:05}-" + "x" * 100)
            mutations.append({"path": str(old), "operation": "rename" if n % 2 else "delete",
                              "result": observe(lambda: os.rename(old, old.with_name(f"moved-{n}")))
                              if n % 2 else observe(lambda: old.unlink())})
    thread = threading.Thread(target=mutate, daemon=True); thread.start()
    traversal = tool(["/usr/bin/find", str(mount / "paged"), "-type", "f", "-delete"], timeout=45)
    thread.join(5)
    remaining = tool(["/usr/bin/find", str(mount / "paged"), "-type", "f"], timeout=15)
    emit({"pagination": {"first": first, "second": second, "delete": removal,
                        "afterDelete": after_delete, "rename": rename, "afterRename": after_rename,
                        "traversal": traversal, "mutations": mutations, "mutationThreadFinished": not thread.is_alive(),
                        "remaining": remaining}})


def second_user(port, secret):
    direct = observe(lambda: Path(secret).read_text())
    emit({"credentialedProcess": {"uid": os.getuid(), "gid": os.getgid(), "pid": os.getpid(),
                                  "groups": os.getgroups(), "directRead": direct}})
    rpc = RPC(port)
    fh = rpc.lookup(rpc.mount(), "owner-secret")
    x = rpc.call(100003, 6, opaque(fh) + u64(0) + u32(128))
    status = x.int(); x.attr()
    data = b""
    if status == 0:
        count, eof, data = x.int(), x.int(), x.blob()
        if count != len(data): raise ValueError("read count mismatch")
    emit({"secondUser": {"pid": os.getpid(), "uid": os.getuid(), "gid": os.getgid(),
                         "groups": os.getgroups(), "directRead": direct,
                         "nfsStatus": status, "nfsData": data.decode(errors="replace"),
                         "ownerBypass": status == 0 and data == b"owner-only-secret\n"}})


def handles(port, old=""):
    rpc = RPC(port)
    root = rpc.mount()
    fh = rpc.lookup(root, "acknowledged.txt")
    prior_status = None
    if old:
        prior_status = rpc.call(100003, 1, opaque(bytes.fromhex(old))).int()
    emit({"handles": {"rootHex": root.hex(), "fileHex": fh.hex(), "oldGetattrStatus": prior_status}})


def cost(root, upper, count, kind, original):
    def inventory(folder):
        return [{'name': p.name, 'size': p.stat().st_size, 'sha256': filehash(p)}
                if p.is_file() and not p.is_symlink() else {'name': p.name, 'kind': 'not-regular-file'}
                for p in sorted(folder.iterdir())]
    base_fixture, visible_fixture = inventory(original), inventory(root)
    emit({'costFixture': {'kind': kind, 'entries': count, 'baseFixture': base_fixture,
                          'visibleFixture': visible_fixture}})
    if len(base_fixture) != count or visible_fixture != base_fixture:
        raise ValueError('visible cost fixture does not match the requested base fixture')
    before = usage(upper) if upper else usage(root)
    start = time.monotonic()
    read = tool(["/usr/bin/find", str(root), "-type", "f", "-exec", "/bin/cat", "{}", "+"], timeout=35)
    after_read = usage(upper) if upper else usage(root)
    # Compile actual C through the measured path. One invocation bounds process overhead.
    sources = sorted(str(p) for p in root.glob("*.c"))
    os.chdir(root)
    compile_result = tool(["/usr/bin/clang", "-fsyntax-only", *sources], timeout=45)
    # A real object output captures mounted write cost without launching thousands of compilers.
    object_result = tool(["/usr/bin/clang", "-c", sources[0], "-o", str(root / "unit.o")], timeout=15)
    before_delete = usage(upper) if upper else usage(root)
    deletion = tool(["/usr/bin/find", str(root), "-type", "f", "-delete"], timeout=45)
    after = usage(upper) if upper else usage(root)
    emit({"cost": {"kind": kind, "entries": count, "baseFixture": base_fixture,
                   "visibleFixture": visible_fixture, "before": before, "afterRead": after_read,
                   "beforeDelete": before_delete, "after": after, "read": read,
                   "compile": compile_result, "objectCompile": object_result, "delete": deletion,
                   "elapsedSeconds": time.monotonic() - start,
                   "errorCount": sum(r["exit"] != 0 for r in [read, compile_result, object_result, deletion]),
                   "baseBytes": usage(original),
                   "copiedBytesMeaning": "Upper byte deltas are observed storage, not transient I/O counters"}})


def clone_tree(source, dest):
    lib = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    clone = lib.clonefile
    clone.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int]
    clone.restype = ctypes.c_int
    dest.mkdir()
    start = time.monotonic(); errors = []
    for p in sorted(source.iterdir()):
        if clone(os.fsencode(p), os.fsencode(dest / p.name), 0):
            errors.append({"path": str(p), "errno": ctypes.get_errno()})
    emit({"clonefile": {"elapsedSeconds": time.monotonic() - start, "errors": errors,
                         "source": usage(source), "destination": usage(dest),
                         "api": "clonefile(3), flags=0; physical shared extents not counted as copied bytes"}})
    if errors: sys.exit(1)


def huge_fixture(base):
    base.mkdir(parents=True)
    chunk = b"a" * 1048576
    start = time.monotonic()
    with (base / "large").open("wb") as f:
        for _ in range(1024): f.write(chunk)
        f.flush(); os.fsync(f.fileno())
    emit({"hugeFixture": usage(base), "elapsedSeconds": time.monotonic() - start,
          "sha256": filehash(base / "large")})


def filehash(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for chunk in iter(lambda: f.read(1048576), b""): h.update(chunk)
    return h.hexdigest()


def staged(upper):
    emit({"staged": [{"path": str(p), "size": p.stat().st_size, "sha256": filehash(p)}
                     for p in upper.rglob("*") if p.is_file() and p.stat().st_size > 1048576]})


def recovered(mount):
    value = (mount / "acknowledged.txt").read_text()
    emit({"recovery": {"contents": value, "preservedAcknowledged": value.startswith("completed-fsynced-write\n"),
                       "caseAlias": observe(lambda: (mount / "casename.txt").read_text()),
                       "unicodeAlias": observe(lambda: (mount / "cafe\u0301.txt").read_text()),
                       "unrelated": (mount / "unrelated.txt").read_text()}})


def truncate(root, upper, kind):
    start = time.monotonic()
    fd = os.open(root / "large", os.O_WRONLY)
    os.ftruncate(fd, 0); os.fsync(fd); os.close(fd)
    emit({"truncate": {"kind": kind, "elapsedSeconds": time.monotonic() - start,
                       "newSize": (root / "large").stat().st_size,
                       "upper": usage(upper) if upper else None,
                       "sourceDerivedCopiedBytes": 1073741824 if kind == "nfs" else 0,
                       "copiedBytesBasis": "Successful inherited copy_up uses io::copy before SETATTR truncation; not a sampled I/O counter"}})


def capabilities(root):
    p = root / "capability"
    p.write_text("capability\n")
    hard = observe(lambda: os.link(p, root / "hard-link"))
    # Python on macOS does not expose os.setxattr. Use the ordinary native tool.
    written = tool(["/usr/bin/xattr", "-w", "com.javelin.suitability", "attribute", str(p)])
    fetched = tool(["/usr/bin/xattr", "-p", "com.javelin.suitability", str(p)])
    xattr = {"ok": written["exit"] == 0, "command": written}
    read = {"ok": fetched["exit"] == 0 and fetched["stdout"].strip() == "attribute", "command": fetched}
    tool(["/bin/ls", "-la@", str(root)])
    emit({"capabilities": {"hardLink": hard, "setxattr": xattr, "getxattr": read,
                           "appleDouble": [p.name for p in root.iterdir() if p.name.startswith("._")]}})


if __name__ == "__main__":
    mode, *args = sys.argv[1:]
    if mode == "fixture": fixture(Path(args[0]))
    elif mode == "aliases": aliases(*map(Path, args))
    elif mode == "held": held(Path(args[0]))
    elif mode == "pagination": pagination(int(args[0]), Path(args[1]))
    elif mode == "second-user": second_user(int(args[0]), args[1])
    elif mode == "cost": cost(Path(args[0]), Path(args[1]) if args[1] else None, int(args[2]), args[3], Path(args[4]))
    elif mode == "clone": clone_tree(*map(Path, args))
    elif mode == "huge-fixture": huge_fixture(Path(args[0]))
    elif mode == "truncate": truncate(Path(args[0]), Path(args[1]) if args[1] else None, args[2])
    elif mode == "capabilities": capabilities(Path(args[0]))
    elif mode == "staged": staged(Path(args[0]))
    elif mode == "recovered": recovered(Path(args[0]))
    elif mode == "handles": handles(int(args[0]), args[1] if len(args) > 1 else "")
    else: raise ValueError(mode)
